//! Comparing the index in use with one rebuilt from scratch.
//!
//! The index is a projection. Everything in it is derived from the wiki and
//! the raw spool, and `reindex` rests on the promise that deleting it loses
//! nothing. That promise is only as good as every path that writes a row also
//! writing its durable copy — and a path that does not fails silently, on the
//! one day it matters: the day the database is gone and the rebuild comes back
//! short.
//!
//! This is how the promise is checked instead of assumed. Rebuild into a
//! scratch index, then hold the two side by side, row by row, in the columns a
//! rebuild is supposed to reproduce. Identifiers are derived, so the same page
//! or observation has the same key in both, and a difference is a fact about
//! the memory rather than about how it was numbered.
//!
//! Some columns are left out on purpose, because a rebuild is not meant to
//! reproduce them: access counts and timestamps belong to use, vectors to
//! whichever model answered, handoffs are consumed once and deliberately not
//! revived. Comparing those would report the design as drift.

use std::collections::BTreeMap;

use anamnesis_core::ids::ProjectId;
use rusqlite::params;
use rusqlite::types::Value;

use crate::Store;

/// One kind of row, held side by side in two indexes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Divergence {
    /// How many rows the index in use holds.
    pub live: usize,
    /// Rows only the index in use holds, by label.
    ///
    /// A rebuild would not produce these, so losing the database loses them.
    pub only_live: Vec<String>,
    /// Rows only the rebuild holds, by label: the index in use is missing
    /// something its sources still have.
    pub only_rebuilt: Vec<String>,
    /// Rows both hold that disagree, by label, with the columns that differ.
    pub differing: Vec<(String, Vec<&'static str>)>,
}

impl Divergence {
    /// Whether the two indexes agree on this kind of row.
    pub fn is_empty(&self) -> bool {
        self.only_live.is_empty() && self.only_rebuilt.is_empty() && self.differing.is_empty()
    }
}

/// Where one project's index differs from a rebuild of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drift {
    /// Pages, compared in every column the markdown sets.
    pub pages: Divergence,
    /// Which names each page is filed under.
    pub entities: Divergence,
    /// Which pages each page links to, and what the link resolved to.
    pub links: Divergence,
    /// Observations, compared whole.
    pub observations: Divergence,
}

impl Drift {
    /// Whether the two indexes agree on everything compared.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
            && self.entities.is_empty()
            && self.links.is_empty()
            && self.observations.is_empty()
    }
}

/// A kind of row: how to read it, and which of its columns must agree.
///
/// Every query takes the project as `?1` and returns the key first, a label a
/// person can find the row by second, and then the compared columns in the
/// order `columns` names them.
struct Rows {
    sql: &'static str,
    columns: &'static [&'static str],
}

const PAGES: Rows = Rows {
    sql: "SELECT id, path,
                 path, title, body, tier, status, pinned, canonical, salience,
                 expires_at, supersedes, supersedes_target, is_latest, session_id
          FROM pages WHERE project_id = ?1",
    columns: &[
        "path",
        "title",
        "body",
        "tier",
        "status",
        "pinned",
        "canonical",
        "salience",
        "expires_at",
        "supersedes",
        "supersedes_target",
        "is_latest",
        "session_id",
    ],
};

// Entity ids are integers each database hands out in its own order, so the
// row is keyed by the name rather than the number.
const ENTITIES: Rows = Rows {
    sql: "SELECT pe.page_id || ' ' || e.name, p.path || ' → ' || e.name
          FROM page_entities pe
          JOIN entities e ON e.id = pe.entity_id
          JOIN pages p ON p.id = pe.page_id
          WHERE p.project_id = ?1",
    columns: &[],
};

const LINKS: Rows = Rows {
    sql: "SELECT l.from_page_id || ' ' || l.to_target, p.path || ' → ' || l.to_target,
                 l.to_page_id, l.to_project_id
          FROM page_links l
          JOIN pages p ON p.id = l.from_page_id
          WHERE p.project_id = ?1",
    columns: &["to_page_id", "to_project_id"],
};

// Joined through sessions because an observation names its session, not its
// project. Session rows themselves are not compared: a session is also
// recorded when it only claims a handoff, which leaves no transcript and is
// not memory, and the state a live session is in is not something a
// transcript records.
const OBSERVATIONS: Rows = Rows {
    sql: "SELECT o.id, substr(o.session_id, 1, 8) || ' ' || o.at || ' ' || o.kind,
                 o.session_id, o.kind, o.tool_name, o.tool_ok, o.tool_call_id, o.at,
                 o.body, o.truncated, o.sanitized
          FROM observations o
          JOIN sessions s ON s.id = o.session_id
          WHERE s.project_id = ?1",
    columns: &[
        "session_id",
        "kind",
        "tool_name",
        "tool_ok",
        "tool_call_id",
        "at",
        "body",
        "truncated",
        "sanitized",
    ],
};

/// A row as read: its label, and its compared columns in order.
type Row = (String, Vec<Value>);

impl Store {
    /// Where this index differs from `rebuilt` for one project.
    ///
    /// `self` is the index in use and `rebuilt` a scratch index built from
    /// the same wiki and spool; the direction matters, because "only here"
    /// means opposite things on the two sides. Reads only — neither index is
    /// written.
    pub fn drift_from(&self, rebuilt: &Store, project: ProjectId) -> crate::Result<Drift> {
        Ok(Drift {
            pages: self.diverge(rebuilt, project, &PAGES)?,
            entities: self.diverge(rebuilt, project, &ENTITIES)?,
            links: self.diverge(rebuilt, project, &LINKS)?,
            observations: self.diverge(rebuilt, project, &OBSERVATIONS)?,
        })
    }

    fn diverge(
        &self,
        rebuilt: &Store,
        project: ProjectId,
        rows: &Rows,
    ) -> crate::Result<Divergence> {
        let live = self.read_rows(project, rows)?;
        let mut other = rebuilt.read_rows(project, rows)?;
        let mut divergence = Divergence {
            live: live.len(),
            ..Divergence::default()
        };

        for (key, (label, values)) in live {
            match other.remove(&key) {
                None => divergence.only_live.push(label),
                Some((_, theirs)) => {
                    let differ: Vec<&'static str> = rows
                        .columns
                        .iter()
                        .zip(values.iter().zip(&theirs))
                        .filter(|(_, (ours, theirs))| ours != theirs)
                        .map(|(column, _)| *column)
                        .collect();
                    if !differ.is_empty() {
                        divergence.differing.push((label, differ));
                    }
                }
            }
        }
        divergence.only_rebuilt = other.into_values().map(|(label, _)| label).collect();

        // Keyed by identifier, which means nothing to a reader; sorted by the
        // label, which for observations is when they happened.
        divergence.only_live.sort();
        divergence.only_rebuilt.sort();
        divergence.differing.sort();
        Ok(divergence)
    }

    fn read_rows(&self, project: ProjectId, rows: &Rows) -> crate::Result<BTreeMap<String, Row>> {
        let conn = self.connection();
        let mut statement = conn.prepare(rows.sql)?;
        let found = statement.query_map(params![project.to_string()], |row| {
            let key: String = row.get(0)?;
            let label: String = row.get(1)?;
            let values = (0..rows.columns.len())
                .map(|index| row.get::<_, Value>(index + 2))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((key, (label, values)))
        })?;
        Ok(found.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::{SessionId, WorkspaceId};
    use anamnesis_core::observation::{BoundedBody, EventKind};
    use anamnesis_core::page::{Frontmatter, Page, PagePath};
    use anamnesis_core::scope::resolve_scope;
    use anamnesis_core::session::AgentKind;
    use jiff::Timestamp;

    fn now() -> Timestamp {
        "2026-09-24T09:00:00Z".parse().expect("timestamp")
    }

    /// Two indexes of the same project, holding the same page and the same
    /// observation — the state a healthy memory is in.
    struct Pair {
        _repo: tempfile::TempDir,
        live: Store,
        rebuilt: Store,
        project: ProjectId,
        session: SessionId,
    }

    fn pair() -> Pair {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");
        let session = SessionId::derive(scope.project_id, "agent-session");

        let open = || {
            let store = Store::open_in_memory().expect("store");
            store.migrate().expect("migrate");
            store.upsert_project(&scope, now()).expect("project");
            store
                .ensure_session(&crate::new_session(
                    session,
                    scope.project_id,
                    WorkspaceId::from_uuid(uuid::Uuid::nil()),
                    AgentKind::ClaudeCode,
                    "/repo".into(),
                    now(),
                    None,
                ))
                .expect("session");
            store
                .upsert_page(&page(scope.project_id, "decisions/a.md", "body"), now())
                .expect("page");
            store
        };

        let pair = Pair {
            live: open(),
            rebuilt: open(),
            project: scope.project_id,
            session,
            _repo: repo,
        };
        let shared = observation(pair.session, "the same event in both");
        pair.live.insert_observation(&shared).expect("live");
        pair.rebuilt.insert_observation(&shared).expect("rebuilt");
        pair
    }

    fn page(project: ProjectId, path: &str, body: &str) -> Page {
        Page::new(
            project,
            PagePath::parse(path).expect("path"),
            Frontmatter::new("A decision", Vec::new()).expect("frontmatter"),
            body,
        )
    }

    fn observation(session: SessionId, body: &str) -> anamnesis_core::observation::Observation {
        crate::new_observation(
            session,
            EventKind::UserPrompt,
            None,
            BoundedBody::truncating(body, 1024),
            now(),
        )
    }

    #[test]
    fn two_indexes_of_the_same_memory_do_not_drift() {
        let pair = pair();

        let drift = pair
            .live
            .drift_from(&pair.rebuilt, pair.project)
            .expect("drift");

        assert!(drift.is_empty(), "{drift:?}");
        assert_eq!(drift.pages.live, 1);
        assert_eq!(drift.observations.live, 1);
    }

    /// The case the whole comparison exists for: a row the index holds and no
    /// transcript does. Nothing else would ever say so — until the database
    /// is lost and the rebuild comes back without it.
    #[test]
    fn an_observation_no_transcript_holds_is_named_as_only_in_the_index() {
        let pair = pair();
        pair.live
            .insert_observation(&observation(pair.session, "never spooled"))
            .expect("insert");

        let drift = pair
            .live
            .drift_from(&pair.rebuilt, pair.project)
            .expect("drift");

        assert_eq!(drift.observations.only_live.len(), 1, "{drift:?}");
        assert!(drift.observations.only_rebuilt.is_empty());
        assert!(drift.pages.is_empty());
    }

    #[test]
    fn a_page_the_index_lost_is_named_as_only_in_the_rebuild() {
        let pair = pair();
        pair.rebuilt
            .upsert_page(&page(pair.project, "gotchas/b.md", "only on disk"), now())
            .expect("page");

        let drift = pair
            .live
            .drift_from(&pair.rebuilt, pair.project)
            .expect("drift");

        assert_eq!(drift.pages.only_rebuilt, ["gotchas/b.md"]);
        assert!(drift.pages.only_live.is_empty());
    }

    /// A stale row is reported by the column that is stale, so the reader
    /// knows whether it is a word in the body or the page's standing.
    #[test]
    fn a_page_that_differs_names_the_columns_that_differ() {
        let pair = pair();
        pair.live
            .upsert_page(
                &page(pair.project, "decisions/a.md", "edited in the index"),
                now(),
            )
            .expect("page");

        let drift = pair
            .live
            .drift_from(&pair.rebuilt, pair.project)
            .expect("drift");

        assert_eq!(
            drift.pages.differing,
            [("decisions/a.md".to_owned(), vec!["body"])]
        );
    }

    /// Entity ids are numbered by each database in its own order; two
    /// indexes that file the same page under the same name agree even when
    /// the numbers do not.
    #[test]
    fn entities_are_compared_by_name_not_by_number() {
        let pair = pair();
        let page_id = anamnesis_core::ids::PageId::derive(
            pair.project,
            &PagePath::parse("decisions/a.md").expect("path"),
        );
        let entity = |name: &str| anamnesis_core::page::Entity::parse(name).expect("entity");
        // The index in use once filed the page under another name first, so
        // `redis` is its second entity and the rebuild's first.
        pair.live
            .set_page_entities(pair.project, page_id, &[entity("kafka")])
            .expect("live");
        pair.live
            .set_page_entities(pair.project, page_id, &[entity("redis")])
            .expect("live");
        pair.rebuilt
            .set_page_entities(pair.project, page_id, &[entity("redis")])
            .expect("rebuilt");

        let drift = pair
            .live
            .drift_from(&pair.rebuilt, pair.project)
            .expect("drift");

        assert!(drift.entities.is_empty(), "{:?}", drift.entities);
    }
}
