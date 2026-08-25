//! Rebuilding the SQLite index from the two things that actually hold the
//! memory: the wiki and the raw spool.
//!
//! The index is disposable by design — page identifiers are derived from
//! `(project, path)` and session identifiers from `(project, agent session
//! id)`, so rebuilding reproduces exactly the same rows rather than a
//! second copy of everything. That is what makes this operation safe to run
//! against a database that still exists, not only a missing one.
//!
//! Two sources, and the split matters:
//!
//! * **`wiki/`** holds the compiled pages, and is the source of truth for
//!   them. A page edited by hand in Obsidian is picked up here the same as
//!   one this system wrote.
//! * **`raw/`** holds the observations those pages were compiled from, which
//!   exist in no wiki.
//!
//! What a rebuild deliberately does **not** restore: pending handoffs. A
//! handoff is produced by consolidation, consumed once, and is a statement
//! about what the *next* session should know — reviving one from a
//! transcript would hand a stale note to whoever starts next, which is worse
//! than starting with none.

use anamnesis_core::observation::EventKind;
use anamnesis_core::scope::ResolvedScope;
use anamnesis_core::session::Session;
use anamnesis_store::{RawRecord, RawSpool, Store};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

/// What a rebuild put back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rebuilt {
    /// Pages indexed from the wiki.
    pub pages: usize,
    /// Sessions recovered from the spool.
    pub sessions: usize,
    /// Observations recovered from the spool.
    pub observations: usize,
    /// Spool files that held no session header, and so could not be attached
    /// to a session. Their observations are counted here rather than
    /// silently dropped from the report.
    pub orphaned_files: usize,
}

/// Rebuild the index for one project from its wiki and spool.
///
/// Idempotent: every identifier involved is derived rather than minted, so
/// running this twice leaves the same rows rather than duplicates.
pub fn rebuild(
    store: &Store,
    wiki: &Wiki,
    raw: &RawSpool,
    scope: &ResolvedScope,
    now: Timestamp,
) -> anyhow::Result<Rebuilt> {
    let mut report = Rebuilt::default();
    store.upsert_project(scope, now)?;

    report.pages = rebuild_pages(store, wiki, scope, now)?;
    let (sessions, observations, orphaned) = rebuild_sessions(store, raw, scope)?;
    report.sessions = sessions;
    report.observations = observations;
    report.orphaned_files = orphaned;

    Ok(report)
}

/// Re-index every page in the wiki, including its entities and links.
///
/// Links are resolved in a second pass over the same pages. One pass cannot
/// do it: a page linking to one written later would resolve against a page
/// that does not exist in the index yet, which is the exact bug the
/// backlink fix in `set_page_links` exists to prevent — and a rebuild must
/// not reintroduce it by visiting pages in an unlucky order.
fn rebuild_pages(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    now: Timestamp,
) -> anyhow::Result<usize> {
    let paths = wiki.pages(&scope.scope)?;
    let mut indexed = Vec::with_capacity(paths.len());

    for path in paths {
        // A page that will not parse is reported and skipped: one malformed
        // file must not cost the rebuild every page after it.
        let parsed = match wiki.read_page(&scope.scope, &path) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(%error, %path, "skipping unreadable page");
                continue;
            }
        };

        let entities = parsed.frontmatter.entities.clone();
        let page = anamnesis_core::page::Page::new(
            scope.project_id,
            path.clone(),
            parsed.frontmatter,
            parsed.body.clone(),
        );
        store.upsert_page(&page, now)?;
        store.set_page_entities(scope.project_id, page.id, &entities)?;
        indexed.push((page.id, parsed.body));
    }

    for (page_id, body) in &indexed {
        store.set_page_links(
            scope.project_id,
            *page_id,
            &anamnesis_wiki::extract_links(body),
        )?;
    }

    Ok(indexed.len())
}

/// Recover sessions and their observations from the spool.
fn rebuild_sessions(
    store: &Store,
    raw: &RawSpool,
    scope: &ResolvedScope,
) -> anyhow::Result<(usize, usize, usize)> {
    let mut sessions = 0;
    let mut observations = 0;
    let mut orphaned = 0;

    for file in raw.files()? {
        let records = raw.read_file(&file)?;

        // The header comes first in every file this crate writes, but it is
        // found by type rather than position so a file that lost its first
        // line is diagnosed rather than misread.
        let Some(session) = records.iter().find_map(|record| match record {
            RawRecord::Session(session) => Some(session.as_ref().clone()),
            RawRecord::Observation(_) => None,
        }) else {
            tracing::warn!(file = %file.display(), "spool file has no session header; skipping");
            orphaned += 1;
            continue;
        };

        // A spool holds every project under one root, so a rebuild scoped to
        // one project has to ignore the rest.
        if session.project_id != scope.project_id {
            continue;
        }

        store.ensure_session(&reopened(&session))?;
        sessions += 1;

        // The header is written once, when the file is created, and the
        // spool is append-only — so it always says the session was open,
        // even for one that ended hours later. The transcript records the
        // ending anyway: a `session-end` observation *is* the record that
        // the session closed, and its timestamp is when.
        let mut ended_at = session.ended_at;
        for record in &records {
            if let RawRecord::Observation(observation) = record {
                store.insert_observation(observation)?;
                observations += 1;
                if observation.kind == EventKind::SessionEnd {
                    ended_at = Some(observation.at);
                }
            }
        }

        // Closed after the observations are in, so the row is complete
        // before it is marked finished.
        if let Some(ended_at) = ended_at {
            store.close_session(session.id, ended_at)?;
        }
    }

    Ok((sessions, observations, orphaned))
}

/// A session as it should be inserted during a rebuild.
///
/// `ensure_session` ignores a row that already exists, so the state a
/// session ends up in is decided by [`rebuild_sessions`] closing it
/// afterwards, not by what the header happened to say.
fn reopened(session: &Session) -> Session {
    let mut session = session.clone();
    session.state = anamnesis_core::session::SessionState::Open;
    session.ended_at = None;
    session
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::observation::BoundedBody;
    use anamnesis_core::page::{Frontmatter, Page, PagePath};
    use anamnesis_core::scope::resolve_scope;
    use anamnesis_core::session::AgentKind;
    use anamnesis_store::{new_observation, new_session};

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        store: Store,
        wiki: Wiki,
        raw: RawSpool,
        scope: ResolvedScope,
    }

    fn harness() -> Harness {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");

        let data = tempfile::tempdir().expect("data");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");

        Harness {
            wiki: Wiki::open(data.path().join("wiki")).expect("wiki"),
            raw: RawSpool::new(data.path().join("raw")),
            store,
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    /// Write a page to the wiki only — as if the index had been lost.
    fn wiki_page(harness: &Harness, path: &str, title: &str, body: &str) {
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            Frontmatter::new(title, Vec::new()).expect("frontmatter"),
            body,
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
    }

    /// Spool a session and its observations, again bypassing the index.
    fn spool_session(harness: &Harness, agent_session: &str, bodies: &[&str]) -> Session {
        let session = new_session(
            anamnesis_core::ids::SessionId::derive(harness.scope.project_id, agent_session),
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        );
        for body in bodies {
            let observation = new_observation(
                session.id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating(*body, 1024),
                now(),
            );
            harness
                .raw
                .append(&harness.scope.scope, &session, &observation)
                .expect("spool");
        }
        session
    }

    #[test]
    fn a_rebuild_recovers_pages_sessions_and_observations() {
        let harness = harness();
        wiki_page(
            &harness,
            "decisions/0001-storage.md",
            "Storage engine",
            "We chose SQLite.",
        );
        spool_session(&harness, "session-1", &["first prompt", "second prompt"]);

        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");

        assert_eq!(report.pages, 1);
        assert_eq!(report.sessions, 1);
        assert_eq!(report.observations, 2);
        assert_eq!(report.orphaned_files, 0);

        // And the rebuilt rows are actually queryable.
        let hits = harness
            .store
            .query_pages(harness.scope.project_id, "sqlite", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Storage engine");
    }

    #[test]
    fn rebuilding_twice_leaves_the_same_rows() {
        // Identifiers are derived, so a second run must not duplicate
        // anything - this is what makes reindex safe on a live database.
        let harness = harness();
        wiki_page(&harness, "notes/a.md", "A", "body");
        spool_session(&harness, "session-1", &["one"]);

        let first = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("first");
        let second = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("second");

        assert_eq!(first, second);
        assert_eq!(
            harness
                .store
                .page_count(harness.scope.project_id)
                .expect("pages"),
            1
        );
        assert_eq!(
            harness
                .store
                .session_count(harness.scope.project_id)
                .expect("sessions"),
            1
        );
        let session = anamnesis_core::ids::SessionId::derive(harness.scope.project_id, "session-1");
        assert_eq!(
            harness.store.observations(session).expect("obs").len(),
            1,
            "observation ids are derived from the record, not minted per run"
        );
    }

    #[test]
    fn supersession_resolves_regardless_of_the_order_pages_are_visited() {
        // `a.md` replaces `z.md`, which sorts last and is therefore indexed
        // after it. The claim is recorded as authored and resolved when the
        // page it names arrives, so a rebuild cannot lose it by walking the
        // wiki in the order the filesystem happens to hand it over.
        let harness = harness();
        let mut frontmatter = Frontmatter::new("A", Vec::new()).expect("frontmatter");
        frontmatter.supersedes = Some(PagePath::parse("z.md").expect("path"));
        let replacement = Page::new(
            harness.scope.project_id,
            PagePath::parse("a.md").expect("path"),
            frontmatter,
            "The page that replaces z.",
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &replacement, "write")
            .expect("write");
        wiki_page(&harness, "z.md", "Z", "The page being replaced.");

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");

        let heads: Vec<String> = harness
            .store
            .connection()
            .prepare("SELECT path FROM pages WHERE is_latest = 1 ORDER BY path")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<String>, _>>()
            .expect("rows");
        assert_eq!(heads, vec!["a.md".to_owned()], "z.md was replaced");
    }

    #[test]
    fn links_resolve_regardless_of_the_order_pages_are_visited() {
        // `a.md` links to `z.md`, which sorts last and is therefore indexed
        // after it. A single-pass rebuild would leave that link dangling.
        let harness = harness();
        wiki_page(&harness, "a.md", "A", "See [[z.md]].");
        wiki_page(&harness, "z.md", "Z", "The target.");

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");

        let unresolved: i64 = harness
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM page_links WHERE to_page_id IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(unresolved, 0, "every link should have resolved");
    }

    #[test]
    fn a_closed_session_comes_back_closed() {
        let harness = harness();
        let mut session = spool_session(&harness, "session-1", &["work"]);

        // Re-spool the header with an end time, the way a closed session's
        // transcript looks.
        session.ended_at = Some("2026-08-25T10:00:00Z".parse().unwrap());
        let path = harness.raw.locate(&harness.scope.scope, &session);
        let text = std::fs::read_to_string(&path).expect("read");
        let rewritten: Vec<String> = text
            .lines()
            .map(|line| {
                if line.contains("\"type\":\"session\"") {
                    serde_json::to_string(&RawRecord::Session(Box::new(session.clone())))
                        .expect("encode")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").expect("write");

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");

        let loaded = harness
            .store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert!(!loaded.is_open());
        assert_eq!(loaded.ended_at, session.ended_at);
    }

    #[test]
    fn a_spool_file_with_no_header_is_reported_not_silently_dropped() {
        let harness = harness();
        let dir = harness
            .raw
            .root()
            .join("default")
            .join("widget")
            .join("2026-08-25");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("orphan.jsonl"),
            "{\"type\":\"observation\",\"id\":\"01a035a6-be79-7c92-8718-3dafc327751d\",\
             \"session_id\":\"01a035a6-be79-7c92-8718-3dafc327751e\",\"kind\":\"user-prompt\",\
             \"tool\":null,\"at\":\"2026-08-25T09:00:00Z\",\
             \"body\":{\"text\":\"lost\",\"truncated\":false},\"sanitized\":true}\n",
        )
        .expect("write");

        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");

        assert_eq!(report.orphaned_files, 1);
        assert_eq!(report.sessions, 0);
    }

    #[test]
    fn a_session_end_in_the_transcript_closes_the_rebuilt_session() {
        // The spool header is written once, at file creation, and never
        // updated - so it always says "open". Without reading the
        // session-end observation, every rebuilt session would come back
        // open, which is what the first real run of `reindex` actually did.
        let harness = harness();
        let session = new_session(
            anamnesis_core::ids::SessionId::derive(harness.scope.project_id, "session-1"),
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        );
        assert!(session.is_open(), "the header is written while still open");

        let ended: Timestamp = "2026-08-25T10:30:00Z".parse().unwrap();
        for (kind, body, at) in [
            (EventKind::UserPrompt, "work", now()),
            (EventKind::SessionEnd, "clear", ended),
        ] {
            let observation = new_observation(
                session.id,
                kind,
                None,
                BoundedBody::truncating(body, 1024),
                at,
            );
            harness
                .raw
                .append(&harness.scope.scope, &session, &observation)
                .expect("spool");
        }

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");

        let loaded = harness
            .store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert!(
            !loaded.is_open(),
            "a session that ended must come back closed"
        );
        assert_eq!(loaded.ended_at, Some(ended), "and at the time it ended");
    }

    #[test]
    fn an_empty_wiki_and_spool_rebuild_to_nothing() {
        let harness = harness();
        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("rebuild");
        assert_eq!(report, Rebuilt::default());
    }
}
