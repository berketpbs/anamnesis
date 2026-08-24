//! Workstream storage: named threads of work, and the sessions and handoffs
//! tied to each one — a workstream's visible event ledger.
//!
//! Everything a workstream *is* lives here in three tables (see
//! `V06__workstreams.sql`): the `workstreams` row itself, the `sessions` that
//! joined it, and the `handoffs` written within it. There is no separate
//! event log to keep in sync — the ledger a caller sees is just those two
//! tables, filtered and ordered.

use anamnesis_core::ids::{ProjectId, SessionId, WorkstreamId};
use anamnesis_core::workstream::{Workstream, WorkstreamSlug, WorkstreamStatus};
use jiff::Timestamp;
use rusqlite::{OptionalExtension, Row, params};

use crate::convert::{parse_id, parse_time};
use crate::{Result, Store};

/// A session that joined a workstream, as it appears in the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkstreamSession {
    /// The session.
    pub session_id: SessionId,
    /// Which harness ran it.
    pub agent: String,
    /// Its lifecycle state at the time of the query.
    pub state: String,
    /// When it started.
    pub started_at: Timestamp,
    /// When it ended, if it has.
    pub ended_at: Option<Timestamp>,
}

/// A handoff written within a workstream, as it appears in the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkstreamHandoff {
    /// Its delivery state (`pending`, `accepted`, or `expired`).
    pub state: String,
    /// When it was written.
    pub created_at: Timestamp,
    /// When it was accepted, if it has been.
    pub accepted_at: Option<Timestamp>,
}

impl Store {
    /// Start (or re-describe) a workstream.
    ///
    /// Idempotent by `(project, slug)`: [`Workstream::new`] derives the same
    /// id for the same slug every time, so calling this twice updates the
    /// title rather than creating a second row — the same idempotency
    /// [`Store::upsert_page`] gives pages.
    pub fn upsert_workstream(&self, workstream: &Workstream) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO workstreams (id, project_id, slug, title, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO UPDATE SET
                 title      = excluded.title,
                 updated_at = excluded.updated_at",
            params![
                workstream.id.to_string(),
                workstream.project_id.to_string(),
                workstream.slug.as_str(),
                workstream.title,
                workstream.status.as_str(),
                workstream.created_at.to_string(),
                workstream.updated_at.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Look up a workstream by its slug.
    pub fn find_workstream(&self, project_id: ProjectId, slug: &str) -> Result<Option<Workstream>> {
        let conn = self.connection();
        conn.query_row(
            "SELECT id, project_id, slug, title, status, created_at, updated_at
             FROM workstreams WHERE project_id = ?1 AND slug = ?2",
            params![project_id.to_string(), slug],
            read_workstream,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Change a workstream's status.
    pub fn set_workstream_status(
        &self,
        id: WorkstreamId,
        status: WorkstreamStatus,
        now: Timestamp,
    ) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE workstreams SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), status.as_str(), now.to_string()],
        )?;
        Ok(())
    }

    /// Every workstream in a project, most recently updated first.
    pub fn list_workstreams(&self, project_id: ProjectId) -> Result<Vec<Workstream>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, project_id, slug, title, status, created_at, updated_at
             FROM workstreams WHERE project_id = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map(params![project_id.to_string()], read_workstream)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The sessions that have joined a workstream, oldest first: one half of
    /// its visible event ledger.
    pub fn workstream_sessions(
        &self,
        workstream_id: WorkstreamId,
    ) -> Result<Vec<WorkstreamSession>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, agent, state, started_at, ended_at
             FROM sessions WHERE workstream_id = ?1 ORDER BY started_at",
        )?;
        let rows = statement.query_map(params![workstream_id.to_string()], |row| {
            Ok(WorkstreamSession {
                session_id: parse_id(row.get(0)?),
                agent: row.get(1)?,
                state: row.get(2)?,
                started_at: parse_time(&row.get::<_, String>(3)?),
                ended_at: row.get::<_, Option<String>>(4)?.map(|t| parse_time(&t)),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The handoffs written within a workstream, newest first: the other
    /// half of its ledger.
    pub fn workstream_handoffs(
        &self,
        workstream_id: WorkstreamId,
    ) -> Result<Vec<WorkstreamHandoff>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT state, created_at, accepted_at
             FROM handoffs WHERE workstream_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map(params![workstream_id.to_string()], |row| {
            Ok(WorkstreamHandoff {
                state: row.get(0)?,
                created_at: parse_time(&row.get::<_, String>(1)?),
                accepted_at: row.get::<_, Option<String>>(2)?.map(|t| parse_time(&t)),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// Build a [`Workstream`] from a joined row.
fn read_workstream(row: &Row<'_>) -> rusqlite::Result<Workstream> {
    Ok(Workstream {
        id: parse_id(row.get(0)?),
        project_id: parse_id(row.get(1)?),
        slug: WorkstreamSlug::parse(&row.get::<_, String>(2)?)
            .expect("stored workstream slug was validated on write"),
        title: row.get(3)?,
        status: WorkstreamStatus::from_storage(&row.get::<_, String>(4)?),
        created_at: parse_time(&row.get::<_, String>(5)?),
        updated_at: parse_time(&row.get::<_, String>(6)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::fixture;
    use anamnesis_core::session::AgentKind;

    fn now() -> Timestamp {
        "2026-08-24T09:00:00Z".parse().expect("timestamp")
    }

    fn workstream(project: ProjectId, slug: &str) -> Workstream {
        Workstream::new(
            project,
            WorkstreamSlug::parse(slug).expect("slug"),
            format!("Title for {slug}"),
            now(),
        )
    }

    #[test]
    fn starting_a_workstream_twice_keeps_one_row() {
        let (_dir, store, project, _workspace) = fixture();
        store
            .upsert_workstream(&workstream(project, "auth-refactor"))
            .unwrap();
        store
            .upsert_workstream(&workstream(project, "auth-refactor"))
            .unwrap();

        assert_eq!(store.list_workstreams(project).unwrap().len(), 1);
    }

    #[test]
    fn a_workstream_is_findable_by_its_slug() {
        let (_dir, store, project, _workspace) = fixture();
        store
            .upsert_workstream(&workstream(project, "auth-refactor"))
            .unwrap();

        let found = store
            .find_workstream(project, "auth-refactor")
            .unwrap()
            .unwrap();
        assert_eq!(found.slug.as_str(), "auth-refactor");
        assert_eq!(found.status, WorkstreamStatus::Active);

        assert!(
            store
                .find_workstream(project, "no-such-slug")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_status_change_is_visible_on_the_next_lookup() {
        let (_dir, store, project, _workspace) = fixture();
        let ws = workstream(project, "auth-refactor");
        store.upsert_workstream(&ws).unwrap();

        store
            .set_workstream_status(ws.id, WorkstreamStatus::Completed, now())
            .unwrap();

        let found = store
            .find_workstream(project, "auth-refactor")
            .unwrap()
            .unwrap();
        assert_eq!(found.status, WorkstreamStatus::Completed);
    }

    #[test]
    fn the_ledger_lists_sessions_and_handoffs_joined_to_a_workstream() {
        let (_dir, store, project, workspace) = fixture();
        let ws = workstream(project, "auth-refactor");
        store.upsert_workstream(&ws).unwrap();

        let session_id = SessionId::derive(project, "agent-session-1");
        store
            .ensure_session(&crate::new_session(
                session_id,
                project,
                workspace,
                AgentKind::ClaudeCode,
                "/repo".into(),
                now(),
                Some(ws.id),
            ))
            .unwrap();
        store
            .record_handoff(&crate::new_handoff(
                project,
                session_id,
                Some(ws.id),
                "carry on",
                now(),
            ))
            .unwrap();

        let sessions = store.workstream_sessions(ws.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);

        let handoffs = store.workstream_handoffs(ws.id).unwrap();
        assert_eq!(handoffs.len(), 1);
        assert_eq!(handoffs[0].state, "pending");
    }

    #[test]
    fn two_workstreams_keep_independent_pending_handoffs() {
        let (_dir, store, project, workspace) = fixture();
        let auth = workstream(project, "auth-refactor");
        let bug = workstream(project, "bug-123");
        store.upsert_workstream(&auth).unwrap();
        store.upsert_workstream(&bug).unwrap();

        let session = |slug: &str| {
            let id = SessionId::derive(project, slug);
            store
                .ensure_session(&crate::new_session(
                    id,
                    project,
                    workspace,
                    AgentKind::ClaudeCode,
                    "/repo".into(),
                    now(),
                    None,
                ))
                .unwrap();
            id
        };
        let auth_writer = session("auth-writer");
        let bug_writer = session("bug-writer");

        store
            .record_handoff(&crate::new_handoff(
                project,
                auth_writer,
                Some(auth.id),
                "auth notes",
                now(),
            ))
            .unwrap();
        store
            .record_handoff(&crate::new_handoff(
                project,
                bug_writer,
                Some(bug.id),
                "bug notes",
                now(),
            ))
            .unwrap();

        // Claiming the auth workstream's handoff must not touch bug-123's.
        let auth_claimant = session("auth-reader");
        let claimed = store
            .claim_handoff(project, auth_claimant, Some(auth.id), now())
            .unwrap();
        assert_eq!(claimed.as_deref(), Some("auth notes"));

        let bug_claimant = session("bug-reader");
        let still_pending = store
            .claim_handoff(project, bug_claimant, Some(bug.id), now())
            .unwrap();
        assert_eq!(still_pending.as_deref(), Some("bug notes"));
    }

    #[test]
    fn a_workstream_handoff_does_not_expire_the_project_wide_one() {
        let (_dir, store, project, workspace) = fixture();
        let ws = workstream(project, "auth-refactor");
        store.upsert_workstream(&ws).unwrap();

        let plain_session = SessionId::derive(project, "plain-writer");
        store
            .ensure_session(&crate::new_session(
                plain_session,
                project,
                workspace,
                AgentKind::ClaudeCode,
                "/repo".into(),
                now(),
                None,
            ))
            .unwrap();
        store
            .record_handoff(&crate::new_handoff(
                project,
                plain_session,
                None,
                "project-wide notes",
                now(),
            ))
            .unwrap();

        let ws_session = SessionId::derive(project, "ws-writer");
        store
            .ensure_session(&crate::new_session(
                ws_session,
                project,
                workspace,
                AgentKind::ClaudeCode,
                "/repo".into(),
                now(),
                Some(ws.id),
            ))
            .unwrap();
        store
            .record_handoff(&crate::new_handoff(
                project,
                ws_session,
                Some(ws.id),
                "ws notes",
                now(),
            ))
            .unwrap();

        let claimant = SessionId::derive(project, "reader");
        store
            .ensure_session(&crate::new_session(
                claimant,
                project,
                workspace,
                AgentKind::ClaudeCode,
                "/repo".into(),
                now(),
                None,
            ))
            .unwrap();

        let plain = store.claim_handoff(project, claimant, None, now()).unwrap();
        assert_eq!(plain.as_deref(), Some("project-wide notes"));
    }
}
