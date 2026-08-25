//! The index side of auto-improve: the signals a pass reads, and the
//! proposals it files.
//!
//! The rules are not here — [`anamnesis_core::improve`] decides what is worth
//! proposing, from plain values. This module supplies those values and keeps
//! the proposals, which mostly means keeping *decisions*: a proposal's
//! identifier is derived from what it is about, so a pass that notices the
//! same condition again lands on the row it already filed. Dismiss one and it
//! stays dismissed.
//!
//! The mirror of that: an open proposal whose condition has stopped holding
//! is resolved, not deleted. Somebody wrote the missing page, or promoted the
//! tier by hand, and the record should say the memory improved rather than
//! quietly forget it was ever asked for.

use std::path::PathBuf;

use anamnesis_core::ids::{PageId, ProjectId, ProposalId};
use anamnesis_core::improve::{
    Facts, MissingTarget, PageStats, Proposal, ProposalKind, ProposalState,
};
use anamnesis_core::page::{PageStatus, Tier};
use anamnesis_core::scope::{ProjectName, Scope, WorkspaceName};
use jiff::Timestamp;
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::convert::{parse_id, parse_page_path, parse_time};
use crate::{Result, Store};

/// A proposal as the index holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredProposal {
    /// Derived from `(project, kind, subject)`.
    pub id: ProposalId,
    /// What it asks for.
    pub kind: ProposalKind,
    /// A page path, or a link target no page answers to.
    pub subject: String,
    /// The page it concerns, when the subject is one that exists.
    pub page_id: Option<PageId>,
    /// The evidence, as of the last pass that saw the condition hold.
    pub rationale: String,
    /// Where it stands.
    pub state: ProposalState,
    /// When it was first filed.
    pub created_at: Timestamp,
    /// When it stopped being open.
    pub decided_at: Option<Timestamp>,
}

/// What one round of filing changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Filed {
    /// Conditions noticed for the first time.
    pub filed: usize,
    /// Conditions that still hold, whose evidence was refreshed.
    pub refreshed: usize,
    /// Open proposals whose condition has stopped holding.
    pub resolved: usize,
}

/// A project, as a scheduler running over all of them sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    /// Identifies the project.
    pub project_id: ProjectId,
    /// Workspace and project names, which is what the wiki is laid out by.
    pub scope: Scope,
    /// Working copy the project was registered from, when it is known.
    pub root: Option<PathBuf>,
    /// When an improvement pass last ran for it.
    pub improved_at: Option<Timestamp>,
}

impl Store {
    /// Everything an improvement pass reads about one project.
    pub fn improve_facts(&self, project_id: ProjectId) -> Result<Facts> {
        Ok(Facts {
            pages: self.page_stats(project_id)?,
            missing: self.missing_targets(project_id)?,
        })
    }

    /// Every page of a project, with the statistics the rules judge.
    fn page_stats(&self, project_id: ProjectId) -> Result<Vec<PageStats>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, path, tier, status, is_latest, updated_at, access_count, last_accessed_at
             FROM pages WHERE project_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], |row| {
            Ok(PageStats {
                page_id: parse_id(row.get::<_, String>(0)?),
                path: parse_page_path(&row.get::<_, String>(1)?),
                tier: Tier::from_storage(&row.get::<_, String>(2)?),
                status: PageStatus::from_storage(&row.get::<_, String>(3)?),
                is_latest: row.get(4)?,
                written_at: parse_time(&row.get::<_, String>(5)?),
                access_count: row.get::<_, i64>(6)?.clamp(0, i64::from(u32::MAX)) as u32,
                last_accessed_at: row.get::<_, Option<String>>(7)?.map(|raw| parse_time(&raw)),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Link targets no page answers to, with the pages asking for them.
    ///
    /// Grouped in Rust rather than in SQL because the group is a list of
    /// paths, and `group_concat` would hand back a string that has to be
    /// split on a separator a page path is allowed to contain.
    fn missing_targets(&self, project_id: ProjectId) -> Result<Vec<MissingTarget>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT l.to_target, p.path
             FROM page_links l
             JOIN pages p ON p.id = l.from_page_id
             WHERE p.project_id = ?1 AND l.to_page_id IS NULL
             ORDER BY l.to_target, p.path",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut targets: Vec<MissingTarget> = Vec::new();
        for row in rows {
            let (target, source) = row?;
            let source = parse_page_path(&source);
            match targets.last_mut() {
                Some(last) if last.target == target => last.sources.push(source),
                _ => targets.push(MissingTarget {
                    target,
                    sources: vec![source],
                }),
            }
        }
        Ok(targets)
    }

    /// File this pass's proposals, and resolve the ones it no longer makes.
    ///
    /// Filing is idempotent: the same condition produces the same identifier,
    /// so a proposal already decided keeps its decision and only an open one
    /// has its evidence refreshed.
    pub fn record_proposals(
        &self,
        project_id: ProjectId,
        proposals: &[Proposal],
        now: Timestamp,
    ) -> Result<Filed> {
        let mut report = Filed::default();
        let mut conn = self.connection();
        let tx = conn.transaction()?;

        let mut current: Vec<Value> = Vec::with_capacity(proposals.len());
        for proposal in proposals {
            let id = ProposalId::derive(project_id, proposal.kind.as_str(), &proposal.subject);
            current.push(Value::Text(id.to_string()));

            // Asked before writing, because what happens depends on the row
            // that is already there: a decided proposal is left exactly as it
            // is, and an upsert cannot express "and tell me which of the two
            // you did" without the timestamp trick that breaks the moment two
            // passes share a clock reading.
            let existing: Option<String> = tx
                .query_row(
                    "SELECT state FROM proposals WHERE id = ?1",
                    params![id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;

            match existing.as_deref() {
                None => {
                    tx.execute(
                        "INSERT INTO proposals
                             (id, project_id, kind, subject, page_id, rationale, state, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7)",
                        params![
                            id.to_string(),
                            project_id.to_string(),
                            proposal.kind.as_str(),
                            proposal.subject,
                            proposal.page_id.map(|id| id.to_string()),
                            proposal.rationale,
                            now.to_string(),
                        ],
                    )?;
                    report.filed += 1;
                }
                Some("open") => {
                    tx.execute(
                        "UPDATE proposals SET rationale = ?2, page_id = ?3 WHERE id = ?1",
                        params![
                            id.to_string(),
                            proposal.rationale,
                            proposal.page_id.map(|id| id.to_string()),
                        ],
                    )?;
                    report.refreshed += 1;
                }
                // Applied, dismissed, or resolved: the decision stands.
                Some(_) => {}
            }
        }

        report.resolved = if current.is_empty() {
            tx.execute(
                "UPDATE proposals SET state = 'resolved', decided_at = ?2
                 WHERE project_id = ?1 AND state = 'open'",
                params![project_id.to_string(), now.to_string()],
            )?
        } else {
            let placeholders = std::iter::repeat_n("?", current.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "UPDATE proposals SET state = 'resolved', decided_at = ?1
                 WHERE project_id = ?2 AND state = 'open' AND id NOT IN ({placeholders})"
            );
            let mut values: Vec<Value> = vec![
                Value::Text(now.to_string()),
                Value::Text(project_id.to_string()),
            ];
            values.extend(current);
            tx.execute(&sql, params_from_iter(values.iter()))?
        };

        tx.commit()?;
        Ok(report)
    }

    /// Proposals for a project, open ones first, newest first within a state.
    pub fn proposals(&self, project_id: ProjectId, open_only: bool) -> Result<Vec<StoredProposal>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, kind, subject, page_id, rationale, state, created_at, decided_at
             FROM proposals
             WHERE project_id = ?1 AND (?2 = 0 OR state = 'open')
             ORDER BY state = 'open' DESC, created_at DESC, subject",
        )?;
        let rows =
            statement.query_map(params![project_id.to_string(), open_only as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;

        let mut proposals = Vec::new();
        for row in rows {
            let (id, kind, subject, page_id, rationale, state, created_at, decided_at) = row?;
            // A kind this build does not know is skipped rather than guessed
            // at: it can only come from a newer version, and acting on it
            // would mean doing something other than what it asked for.
            let Some(kind) = ProposalKind::from_storage(&kind) else {
                continue;
            };
            proposals.push(StoredProposal {
                id: parse_id(id),
                kind,
                subject,
                page_id: page_id.map(parse_id),
                rationale,
                state: ProposalState::from_storage(&state),
                created_at: parse_time(&created_at),
                decided_at: decided_at.as_deref().map(parse_time),
            });
        }
        Ok(proposals)
    }

    /// Proposals whose identifier starts with `prefix`.
    ///
    /// Returns every match rather than the first, so a caller can refuse an
    /// ambiguous prefix instead of acting on whichever row sorted first.
    pub fn proposals_matching(
        &self,
        project_id: ProjectId,
        prefix: &str,
    ) -> Result<Vec<StoredProposal>> {
        let prefix = prefix.to_lowercase();
        Ok(self
            .proposals(project_id, false)?
            .into_iter()
            .filter(|proposal| proposal.id.to_string().starts_with(&prefix))
            .collect())
    }

    /// Record a decision about a proposal.
    ///
    /// Only an open proposal can be decided; returns whether one was.
    pub fn decide_proposal(
        &self,
        id: ProposalId,
        state: ProposalState,
        now: Timestamp,
    ) -> Result<bool> {
        let conn = self.connection();
        let changed = conn.execute(
            "UPDATE proposals SET state = ?2, decided_at = ?3
             WHERE id = ?1 AND state = 'open'",
            params![id.to_string(), state.as_str(), now.to_string()],
        )?;
        Ok(changed > 0)
    }

    /// Every project in the index.
    pub fn projects(&self) -> Result<Vec<ProjectRow>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, workspace, name, root_path, improved_at FROM projects ORDER BY workspace, name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;

        let mut projects = Vec::new();
        for row in rows {
            let (id, workspace, name, root, improved_at) = row?;
            // Names were validated before they were stored; one that no
            // longer parses means a row this crate did not write, and there
            // is no scope to act on.
            let (Ok(workspace), Ok(project)) =
                (WorkspaceName::parse(&workspace), ProjectName::parse(&name))
            else {
                continue;
            };
            projects.push(ProjectRow {
                project_id: parse_id(id),
                scope: Scope { workspace, project },
                root: root.map(PathBuf::from),
                improved_at: improved_at.as_deref().map(parse_time),
            });
        }
        Ok(projects)
    }

    /// Record that an improvement pass just ran for a project.
    pub fn mark_improved(&self, project_id: ProjectId, now: Timestamp) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE projects SET improved_at = ?2 WHERE id = ?1",
            params![project_id.to_string(), now.to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::improve::propose;
    use anamnesis_core::page::{Frontmatter, Page, PagePath};

    use crate::convert::{fixture, fixture_now};

    fn days_ago(days: i64) -> Timestamp {
        fixture_now() - jiff::Span::new().hours(days * 24)
    }

    /// Write a page as it stood `age` days ago, read `reads` times.
    fn page(store: &Store, project: ProjectId, path: &str, age: i64, reads: u32) -> Page {
        let mut frontmatter = Frontmatter::new("A page", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Episodic;
        let page = Page::new(
            project,
            PagePath::parse(path).expect("path"),
            frontmatter,
            "Body about sqlite.",
        );
        store.upsert_page(&page, days_ago(age)).expect("upsert");
        for _ in 0..reads {
            store.record_access(page.id, days_ago(1)).expect("access");
        }
        page
    }

    fn proposals_now(store: &Store, project: ProjectId) -> Vec<Proposal> {
        propose(&store.improve_facts(project).expect("facts"), fixture_now())
    }

    #[test]
    fn the_facts_a_pass_reads_come_back_from_the_index() {
        let (_dir, store, project, _workspace) = fixture();
        let read = page(&store, project, "sessions/read.md", 40, 4);
        page(&store, project, "sessions/quiet.md", 40, 0);

        let facts = store.improve_facts(project).expect("facts");
        assert_eq!(facts.pages.len(), 2);

        let stats = facts
            .pages
            .iter()
            .find(|p| p.page_id == read.id)
            .expect("the page that was read");
        assert_eq!(stats.access_count, 4);
        assert_eq!(stats.last_accessed_at, Some(days_ago(1)));
        assert_eq!(stats.written_at, days_ago(40));
        assert!(stats.is_latest);
    }

    #[test]
    fn unresolved_links_are_grouped_by_the_page_they_ask_for() {
        let (_dir, store, project, _workspace) = fixture();
        let first = page(&store, project, "sessions/a.md", 1, 0);
        let second = page(&store, project, "sessions/b.md", 1, 0);
        store
            .set_page_links(
                project,
                first.id,
                &[
                    "gotchas/windows-bom.md".to_owned(),
                    "notes/solo.md".to_owned(),
                ],
            )
            .expect("links");
        store
            .set_page_links(project, second.id, &["gotchas/windows-bom.md".to_owned()])
            .expect("links");

        let missing = store.improve_facts(project).expect("facts").missing;
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].target, "gotchas/windows-bom.md");
        assert_eq!(missing[0].sources.len(), 2);
        assert_eq!(missing[1].target, "notes/solo.md");
        assert_eq!(missing[1].sources.len(), 1);
    }

    #[test]
    fn a_link_that_resolves_is_not_a_gap() {
        let (_dir, store, project, _workspace) = fixture();
        let source = page(&store, project, "sessions/a.md", 1, 0);
        page(&store, project, "decisions/0001-storage.md", 1, 0);
        store
            .set_page_links(
                project,
                source.id,
                &["decisions/0001-storage.md".to_owned()],
            )
            .expect("links");

        assert!(
            store
                .improve_facts(project)
                .expect("facts")
                .missing
                .is_empty()
        );
    }

    #[test]
    fn filing_the_same_proposal_twice_leaves_one_row() {
        let (_dir, store, project, _workspace) = fixture();
        page(&store, project, "sessions/read.md", 40, 4);

        let first = store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        assert_eq!(first.filed, 1);

        let second = store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        assert_eq!(second.filed, 0);
        assert_eq!(second.refreshed, 1);
        assert_eq!(store.proposals(project, true).expect("list").len(), 1);
    }

    #[test]
    fn a_dismissed_proposal_is_never_filed_again() {
        let (_dir, store, project, _workspace) = fixture();
        page(&store, project, "sessions/read.md", 40, 4);
        store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");

        let open = store.proposals(project, true).expect("list");
        assert!(
            store
                .decide_proposal(open[0].id, ProposalState::Dismissed, fixture_now())
                .expect("decide")
        );

        // The condition still holds, and the pass still notices it — but the
        // decision is attached to the same derived row, so nothing reopens.
        let again = store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        assert_eq!(again.filed, 0);
        assert_eq!(again.refreshed, 0);
        assert!(store.proposals(project, true).expect("list").is_empty());
        assert_eq!(
            store.proposals(project, false).expect("list")[0].state,
            ProposalState::Dismissed
        );
    }

    #[test]
    fn an_open_proposal_whose_condition_is_gone_is_resolved() {
        let (_dir, store, project, _workspace) = fixture();
        let read = page(&store, project, "sessions/read.md", 40, 4);
        store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        assert_eq!(store.proposals(project, true).expect("list").len(), 1);

        // Somebody promoted it by hand.
        let mut promoted = read.clone();
        promoted.frontmatter.tier = Tier::Semantic;
        store.upsert_page(&promoted, fixture_now()).expect("upsert");

        let after = store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        assert_eq!(after.resolved, 1);
        assert!(store.proposals(project, true).expect("list").is_empty());
        assert_eq!(
            store.proposals(project, false).expect("list")[0].state,
            ProposalState::Resolved
        );
    }

    #[test]
    fn a_decided_proposal_is_not_resolved_out_from_under_its_decision() {
        let (_dir, store, project, _workspace) = fixture();
        page(&store, project, "sessions/read.md", 40, 4);
        store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        let open = store.proposals(project, true).expect("list");
        store
            .decide_proposal(open[0].id, ProposalState::Applied, fixture_now())
            .expect("decide");

        store
            .record_proposals(project, &[], fixture_now())
            .expect("record");

        assert_eq!(
            store.proposals(project, false).expect("list")[0].state,
            ProposalState::Applied,
            "a proposal that was carried out stays carried out"
        );
    }

    #[test]
    fn only_open_proposals_can_be_decided() {
        let (_dir, store, project, _workspace) = fixture();
        page(&store, project, "sessions/read.md", 40, 4);
        store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        let id = store.proposals(project, true).expect("list")[0].id;

        assert!(
            store
                .decide_proposal(id, ProposalState::Applied, fixture_now())
                .expect("first")
        );
        assert!(
            !store
                .decide_proposal(id, ProposalState::Dismissed, fixture_now())
                .expect("second"),
            "a decision is made once"
        );
    }

    #[test]
    fn a_prefix_finds_a_proposal_and_reports_every_match() {
        let (_dir, store, project, _workspace) = fixture();
        page(&store, project, "sessions/read.md", 40, 4);
        store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");
        let id = store.proposals(project, true).expect("list")[0]
            .id
            .to_string();

        let found = store
            .proposals_matching(project, &id[..8])
            .expect("matching");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id.to_string(), id);

        assert!(
            store
                .proposals_matching(project, "ffffffff")
                .expect("matching")
                .is_empty()
        );
    }

    #[test]
    fn sweeping_a_page_takes_its_proposals_with_it() {
        let (_dir, store, project, _workspace) = fixture();
        let read = page(&store, project, "sessions/read.md", 40, 4);
        store
            .record_proposals(project, &proposals_now(&store, project), fixture_now())
            .expect("record");

        store.delete_page(read.id).expect("delete");
        assert!(
            store.proposals(project, false).expect("list").is_empty(),
            "there is nothing left to promote"
        );
    }

    #[test]
    fn a_project_remembers_where_it_lives_and_when_it_was_improved() {
        let (dir, store, project, _workspace) = fixture();

        let listed = store.projects().expect("projects");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].project_id, project);
        assert_eq!(listed[0].scope.project.as_str(), "widget");
        let scope = anamnesis_core::scope::resolve_scope(dir.path()).expect("scope");
        assert_eq!(
            listed[0].root,
            Some(scope.root),
            "the working copy the project was registered from"
        );
        assert_eq!(listed[0].improved_at, None);

        store.mark_improved(project, fixture_now()).expect("mark");
        assert_eq!(
            store.projects().expect("projects")[0].improved_at,
            Some(fixture_now())
        );
    }
}
