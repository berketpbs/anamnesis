//! Removing one project's memory, all of it.
//!
//! `forget` takes a page and `forget-session` takes a session. This is the end
//! of that family: the project, everything recorded under it, and the row that
//! says it exists. It is for the memory that is wrong rather than incomplete —
//! a repository re-scoped by accident, a bootstrap run against the wrong
//! directory, a project that was never meant to be remembered — where fixing
//! it page by page is worse than starting the project again.
//!
//! The schema does the work. Every project-scoped table cascades from
//! `projects`, and the tables under *those* cascade in turn, so one delete
//! takes the sessions, their observations, the pages, their entities, links,
//! embeddings and feedback, the handoffs, the workstreams and the proposals.
//! The FTS index goes with the pages through its own trigger.
//!
//! One table does not: `audit_log` has no foreign key, deliberately. A purge
//! is a deliberate change like any other, and the line saying it happened has
//! to outlive the thing it happened to — otherwise the one question somebody
//! asks afterwards, *where did this project's memory go*, has no answer.

use anamnesis_core::ids::ProjectId;
use rusqlite::params;

use crate::Store;

/// What a project's memory consists of, for a report or a receipt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Purged {
    /// Pages in the index.
    pub pages: i64,
    /// Sessions recorded.
    pub sessions: i64,
    /// Observations across those sessions.
    pub observations: i64,
    /// Handoffs, taken and waiting.
    pub handoffs: i64,
    /// Named threads of work.
    pub workstreams: i64,
    /// Proposals, decided and open.
    pub proposals: i64,
}

impl Purged {
    /// Whether there is anything there at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Store {
    /// Count what a purge would take, changing nothing.
    ///
    /// Counted before the delete rather than reported from the delete's own
    /// row counts, because a cascade reports only the rows the statement
    /// touched directly: `DELETE FROM projects` says "1" whatever went with
    /// it, and "1 row removed" is not a description of losing a year of
    /// sessions.
    pub fn purge_preview(&self, project_id: ProjectId) -> crate::Result<Purged> {
        let conn = self.connection();
        let id = project_id.to_string();
        let count = |sql: &str| -> crate::Result<i64> {
            Ok(conn.query_row(sql, params![id], |row| row.get(0))?)
        };

        Ok(Purged {
            pages: count("SELECT COUNT(*) FROM pages WHERE project_id = ?1")?,
            sessions: count("SELECT COUNT(*) FROM sessions WHERE project_id = ?1")?,
            observations: count(
                "SELECT COUNT(*) FROM observations o
                 JOIN sessions s ON s.id = o.session_id
                 WHERE s.project_id = ?1",
            )?,
            handoffs: count("SELECT COUNT(*) FROM handoffs WHERE project_id = ?1")?,
            workstreams: count("SELECT COUNT(*) FROM workstreams WHERE project_id = ?1")?,
            proposals: count("SELECT COUNT(*) FROM proposals WHERE project_id = ?1")?,
        })
    }

    /// Remove the project row, and with it everything that cascades from it.
    ///
    /// Returns what was there, counted before it went.
    pub fn purge_project(&self, project_id: ProjectId) -> crate::Result<Purged> {
        let counted = self.purge_preview(project_id)?;
        let conn = self.connection();
        conn.execute(
            "DELETE FROM projects WHERE id = ?1",
            params![project_id.to_string()],
        )?;
        Ok(counted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::audit::{Action, AuditEntry, Via};
    use anamnesis_core::page::{Frontmatter, Page, PagePath};
    use anamnesis_core::scope::{ResolvedScope, resolve_scope};
    use anamnesis_core::session::AgentKind;
    use jiff::Timestamp;

    fn at(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    /// A project with one of everything, so a purge has something of each kind
    /// to take and a neighbour has something to keep.
    ///
    /// Built through the same path capture uses — a marker file, a resolved
    /// scope, `upsert_project` — because a project assembled by hand-written
    /// `INSERT`s is a project the real code has never seen.
    fn populated(store: &Store, name: &str) -> (tempfile::TempDir, ResolvedScope) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".anamnesis.toml"),
            format!("[scope]\nworkspace = \"default\"\nproject = \"{name}\"\n"),
        )
        .expect("marker");
        let scope = resolve_scope(dir.path()).expect("scope");
        store
            .upsert_project(&scope, at("2026-09-02T09:00:00Z"))
            .expect("project");

        let session = crate::new_session(
            anamnesis_core::ids::SessionId::new(),
            scope.project_id,
            scope.workspace_id,
            AgentKind::ClaudeCode,
            dir.path().to_path_buf(),
            at("2026-09-02T09:00:00Z"),
            None,
        );
        store.ensure_session(&session).expect("session");
        store
            .insert_observation(&crate::new_observation(
                session.id,
                anamnesis_core::observation::EventKind::UserPrompt,
                None,
                anamnesis_core::observation::BoundedBody::truncating("a prompt", 1024),
                at("2026-09-02T09:01:00Z"),
            ))
            .expect("observation");

        let page = Page::new(
            scope.project_id,
            PagePath::parse(&format!("notes/{name}.md")).expect("path"),
            Frontmatter::new("A page", Vec::new()).expect("frontmatter"),
            "what was decided",
        );
        store
            .upsert_page(&page, at("2026-09-02T09:02:00Z"))
            .expect("page");

        (dir, scope)
    }

    fn store() -> Store {
        let store = Store::open_in_memory().expect("store");
        store.migrate().expect("migrate");
        store
    }

    #[test]
    fn a_purge_takes_everything_the_project_had() {
        let store = store();
        let (_dir, scope) = populated(&store, "widget");
        let id = scope.project_id;

        let preview = store.purge_preview(id).expect("preview");
        assert_eq!(preview.pages, 1);
        assert_eq!(preview.sessions, 1);
        assert_eq!(preview.observations, 1);

        let purged = store.purge_project(id).expect("purge");
        assert_eq!(purged, preview, "the receipt did not match the preview");

        assert_eq!(store.page_count(id).expect("pages"), 0);
        assert_eq!(store.session_count(id).expect("sessions"), 0);
        assert!(
            store.purge_preview(id).expect("preview").is_empty(),
            "something survived the purge"
        );
    }

    /// One server holds many projects. A purge that took a neighbour's memory
    /// with it would be the worst kind of bug in this system: silent, total,
    /// and discovered by somebody who did not run the command.
    #[test]
    fn a_purge_leaves_every_other_project_alone() {
        let store = store();
        let (_mine_dir, mine) = populated(&store, "widget");
        let (_theirs_dir, theirs) = populated(&store, "gadget");

        store.purge_project(mine.project_id).expect("purge");

        let left = store.purge_preview(theirs.project_id).expect("preview");
        assert_eq!(left.pages, 1);
        assert_eq!(left.sessions, 1);
        assert_eq!(left.observations, 1);
    }

    /// The one table that does not cascade. Somebody asking where a project's
    /// memory went is asking after the project is gone, and the log has to be
    /// able to answer.
    #[test]
    fn the_record_of_the_purge_outlives_the_project() {
        let store = store();
        let (_dir, scope) = populated(&store, "widget");
        let id = scope.project_id;

        store.purge_project(id).expect("purge");
        store
            .append_audit(
                &AuditEntry::new(
                    Action::Purged,
                    Via::Cli,
                    "default/widget",
                    at("2026-09-02T09:03:00Z"),
                )
                .in_project(id)
                .saying("1 page, 1 session, 1 observation"),
            )
            .expect("append");

        let trail = store.audit_trail(Some(id), 10).expect("trail");
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].action, Action::Purged);
        assert!(trail[0].subject.contains("widget"), "{:?}", trail[0]);
    }

    /// Counted before, not reported from the delete: a cascade only reports
    /// the rows the statement touched, and "1 row removed" is not a
    /// description of losing a year of sessions.
    #[test]
    fn an_empty_project_is_reported_as_empty_rather_than_as_one_row() {
        let store = store();
        let (_dir, scope) = populated(&store, "widget");
        store.purge_project(scope.project_id).expect("purge");

        let nothing = store.purge_preview(scope.project_id).expect("preview");

        assert!(nothing.is_empty());
    }
}
