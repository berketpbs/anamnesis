//! Typed reads and writes over the schema.
//!
//! Everything here is synchronous and short. The capture path runs on every
//! tool call an agent makes, so a write that takes milliseconds is a write the
//! agent never notices, and one that takes hundreds is a stutter in someone's
//! editing session.

use anamnesis_core::handoff::{Handoff, HandoffState};
use anamnesis_core::ids::{HandoffId, ObservationId, ProjectId, SessionId};
use anamnesis_core::observation::{BoundedBody, EventKind, Observation, ToolRef};
use anamnesis_core::page::Page;
use anamnesis_core::session::{AgentKind, Session, SessionState};
use jiff::Timestamp;
use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store};

impl Store {
    /// Record a session, ignoring the call if it is already known.
    ///
    /// Hooks arrive out of order and more than once; the first event of a
    /// session is not reliably `SessionStart`. Making this idempotent means the
    /// capture path never has to ask whether a session exists yet.
    pub fn ensure_session(&self, session: &Session) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO sessions (id, project_id, agent, checkout_path, state, started_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO NOTHING",
            params![
                session.id.to_string(),
                session.project_id.to_string(),
                session.agent.as_str(),
                session.checkout_path.to_string_lossy(),
                state_str(session.state),
                session.started_at.to_string(),
                session.ended_at.map(|t| t.to_string()),
            ],
        )?;
        Ok(())
    }

    /// Mark a session ended.
    pub fn close_session(&self, id: SessionId, ended_at: Timestamp) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE sessions SET state = 'closed', ended_at = ?2 WHERE id = ?1",
            params![id.to_string(), ended_at.to_string()],
        )?;
        Ok(())
    }

    /// Load one session, with the workspace reached through its project.
    pub fn load_session(&self, id: SessionId) -> Result<Option<Session>> {
        let conn = self.connection();
        let session = conn
            .query_row(
                "SELECT s.id, s.project_id, p.workspace_id, s.agent, s.checkout_path,
                        s.state, s.started_at, s.ended_at
                 FROM sessions s
                 JOIN projects p ON p.id = s.project_id
                 WHERE s.id = ?1",
                params![id.to_string()],
                read_session,
            )
            .optional()?;
        Ok(session)
    }

    /// Append an observation.
    pub fn insert_observation(&self, observation: &Observation) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO observations
                 (id, session_id, kind, tool_name, tool_ok, at, body, truncated, sanitized)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (id) DO NOTHING",
            params![
                observation.id.to_string(),
                observation.session_id.to_string(),
                observation.kind.as_str(),
                observation.tool.as_ref().map(|t| t.name.clone()),
                observation.tool.as_ref().and_then(|t| t.ok),
                observation.at.to_string(),
                observation.body.as_str(),
                observation.body.is_truncated(),
                observation.sanitized,
            ],
        )?;
        Ok(())
    }

    /// Every observation in a session, oldest first.
    pub fn observations(&self, session_id: SessionId) -> Result<Vec<Observation>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT id, session_id, kind, tool_name, tool_ok, at, body, truncated, sanitized
             FROM observations WHERE session_id = ?1 ORDER BY at, rowid",
        )?;
        let rows = statement.query_map(params![session_id.to_string()], read_observation)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Insert or refresh the index row for a page.
    pub fn upsert_page(&self, page: &Page, now: Timestamp) -> Result<()> {
        let conn = self.connection();
        let fm = &page.frontmatter;
        conn.execute(
            "INSERT INTO pages
                 (id, project_id, path, title, body, tier, status, pinned, canonical,
                  salience, expires_at, git_commit, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
             ON CONFLICT (id) DO UPDATE SET
                 title      = excluded.title,
                 body       = excluded.body,
                 tier       = excluded.tier,
                 status     = excluded.status,
                 pinned     = excluded.pinned,
                 canonical  = excluded.canonical,
                 salience   = excluded.salience,
                 expires_at = excluded.expires_at,
                 git_commit = excluded.git_commit,
                 updated_at = excluded.updated_at",
            params![
                page.id.to_string(),
                page.project_id.to_string(),
                page.path.as_str(),
                fm.title,
                page.body,
                fm.tier.as_str(),
                status_str(fm.status),
                fm.pinned,
                fm.canonical,
                fm.salience,
                fm.expires_at.map(|t| t.to_string()),
                page.git_commit.clone(),
                now.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Record a handoff, expiring any that was still waiting.
    ///
    /// Expiring first is what upholds the single-pending invariant the schema
    /// enforces: a newer summary always replaces an unread older one, because
    /// handing a session two "where you left off" notes is worse than handing
    /// it the wrong one.
    pub fn record_handoff(&self, handoff: &Handoff) -> Result<()> {
        let mut conn = self.connection();
        let transaction = conn.transaction()?;
        transaction.execute(
            "UPDATE handoffs SET state = 'expired'
             WHERE project_id = ?1 AND state = 'pending'",
            params![handoff.project_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO handoffs (id, project_id, from_session, body, state, created_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![
                handoff.id.to_string(),
                handoff.project_id.to_string(),
                handoff.from_session.to_string(),
                handoff.body.as_str(),
                handoff.created_at.to_string(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Claim the pending handoff for a project, if there is one.
    ///
    /// Claiming is a single statement so two sessions starting at the same
    /// moment cannot both receive it: the second finds nothing pending.
    pub fn claim_handoff(
        &self,
        project_id: ProjectId,
        claimant: SessionId,
        now: Timestamp,
    ) -> Result<Option<String>> {
        let conn = self.connection();
        let body = conn
            .query_row(
                "UPDATE handoffs SET state = 'accepted', to_session = ?2, accepted_at = ?3
                 WHERE id = (
                     SELECT id FROM handoffs
                     WHERE project_id = ?1 AND state = 'pending'
                     ORDER BY created_at DESC LIMIT 1
                 )
                 RETURNING body",
                params![project_id.to_string(), claimant.to_string(), now.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(body)
    }

    /// How many sessions a project has recorded.
    pub fn session_count(&self, project_id: ProjectId) -> Result<i64> {
        let conn = self.connection();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?)
    }

    /// How many pages a project has indexed.
    pub fn page_count(&self, project_id: ProjectId) -> Result<i64> {
        let conn = self.connection();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM pages WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?)
    }
}

/// Build a [`Session`] from a joined row.
fn read_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: parse_id(row.get::<_, String>(0)?),
        project_id: parse_id(row.get::<_, String>(1)?),
        workspace_id: parse_id(row.get::<_, String>(2)?),
        agent: row
            .get::<_, String>(3)?
            .parse()
            .expect("AgentKind parsing is infallible"),
        checkout_path: row.get::<_, String>(4)?.into(),
        state: parse_state(&row.get::<_, String>(5)?),
        started_at: parse_time(&row.get::<_, String>(6)?),
        ended_at: row
            .get::<_, Option<String>>(7)?
            .map(|t| parse_time(&t)),
    })
}

/// Build an [`Observation`] from a row.
fn read_observation(row: &Row<'_>) -> rusqlite::Result<Observation> {
    let tool_name: Option<String> = row.get(3)?;
    Ok(Observation {
        id: parse_id(row.get::<_, String>(0)?),
        session_id: parse_id(row.get::<_, String>(1)?),
        kind: parse_kind(&row.get::<_, String>(2)?),
        tool: tool_name.map(|name| ToolRef {
            name,
            ok: row.get(4).unwrap_or(None),
        }),
        at: parse_time(&row.get::<_, String>(5)?),
        body: BoundedBody::from_stored(row.get::<_, String>(6)?, row.get(7)?),
        sanitized: row.get(8)?,
    })
}

/// Parse an identifier written by this crate.
///
/// A value here that is not a UUID means the database was edited by hand or
/// corrupted; there is no meaningful recovery, and continuing with a nil id
/// would silently attach data to the wrong row.
fn parse_id<T: std::str::FromStr>(raw: String) -> T
where
    T::Err: std::fmt::Debug,
{
    raw.parse()
        .unwrap_or_else(|error| panic!("stored identifier {raw:?} is not a uuid: {error:?}"))
}

/// Parse a timestamp written by this crate.
fn parse_time(raw: &str) -> Timestamp {
    raw.parse()
        .unwrap_or_else(|error| panic!("stored timestamp {raw:?} is not RFC 3339: {error:?}"))
}

/// Map a stored session state back to its variant.
fn parse_state(raw: &str) -> SessionState {
    match raw {
        "ending" => SessionState::Ending,
        "closed" => SessionState::Closed,
        _ => SessionState::Open,
    }
}

/// Map a stored event kind back to its variant.
fn parse_kind(raw: &str) -> EventKind {
    match raw {
        "session-start" => EventKind::SessionStart,
        "user-prompt" => EventKind::UserPrompt,
        "tool-use" => EventKind::ToolUse,
        "pre-compact" => EventKind::PreCompact,
        "post-compact" => EventKind::PostCompact,
        "session-end" => EventKind::SessionEnd,
        _ => EventKind::Notification,
    }
}

/// Storage form of a session state.
fn state_str(state: SessionState) -> &'static str {
    match state {
        SessionState::Open => "open",
        SessionState::Ending => "ending",
        SessionState::Closed => "closed",
    }
}

/// Storage form of a page status.
fn status_str(status: anamnesis_core::page::PageStatus) -> &'static str {
    use anamnesis_core::page::PageStatus;
    match status {
        PageStatus::Active => "active",
        PageStatus::Historical => "historical",
        PageStatus::DoNotAnswerFrom => "do-not-answer-from",
        PageStatus::Superseded => "superseded",
    }
}

/// Build a handoff ready to be recorded.
pub fn new_handoff(
    project_id: ProjectId,
    from_session: SessionId,
    body: &str,
    created_at: Timestamp,
) -> Handoff {
    Handoff {
        id: HandoffId::new(),
        project_id,
        from_session,
        to_session: None,
        body: BoundedBody::truncating(body, BoundedBody::DEFAULT_LIMIT),
        created_at,
        accepted_at: None,
        state: HandoffState::Pending,
    }
}

/// Build an observation ready to be inserted.
pub fn new_observation(
    session_id: SessionId,
    kind: EventKind,
    tool: Option<ToolRef>,
    body: BoundedBody,
    at: Timestamp,
) -> Observation {
    Observation {
        id: ObservationId::new(),
        session_id,
        kind,
        tool,
        at,
        body,
        sanitized: true,
    }
}

/// Build a session ready to be recorded.
pub fn new_session(
    id: SessionId,
    project_id: ProjectId,
    workspace_id: anamnesis_core::ids::WorkspaceId,
    agent: AgentKind,
    checkout_path: std::path::PathBuf,
    started_at: Timestamp,
) -> Session {
    Session {
        id,
        agent,
        workspace_id,
        project_id,
        checkout_path,
        started_at,
        ended_at: None,
        state: SessionState::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::WorkspaceId;
    use anamnesis_core::scope::resolve_scope;

    fn fixture() -> (tempfile::TempDir, Store, ProjectId, WorkspaceId) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(dir.path()).expect("scope");

        let store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        store.upsert_project(&scope, now()).expect("project");

        (dir, store, scope.project_id, scope.workspace_id)
    }

    fn now() -> Timestamp {
        "2026-08-19T09:00:00Z".parse().expect("timestamp")
    }

    fn session_for(project: ProjectId, workspace: WorkspaceId) -> Session {
        new_session(
            SessionId::derive(project, "agent-session-1"),
            project,
            workspace,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
        )
    }

    #[test]
    fn ensuring_a_session_twice_keeps_one_row() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);

        store.ensure_session(&session).expect("first");
        store.ensure_session(&session).expect("second");

        assert_eq!(store.session_count(project).expect("count"), 1);
    }

    #[test]
    fn a_session_round_trips_with_its_workspace() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("insert");

        let loaded = store.load_session(session.id).expect("load").expect("found");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.agent, AgentKind::ClaudeCode);
        // The workspace was never stored on the session; it came back through
        // the project, which is why it cannot disagree.
        assert_eq!(loaded.workspace_id, workspace);
        assert!(loaded.is_open());
    }

    #[test]
    fn observations_come_back_in_the_order_they_happened() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        for (index, kind) in [
            EventKind::SessionStart,
            EventKind::UserPrompt,
            EventKind::ToolUse,
        ]
        .into_iter()
        .enumerate()
        {
            let at: Timestamp = format!("2026-08-19T09:0{index}:00Z").parse().expect("time");
            store
                .insert_observation(&new_observation(
                    session.id,
                    kind,
                    None,
                    BoundedBody::truncating(format!("event {index}"), 1024),
                    at,
                ))
                .expect("insert");
        }

        let loaded = store.observations(session.id).expect("load");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].kind, EventKind::SessionStart);
        assert_eq!(loaded[2].kind, EventKind::ToolUse);
        assert_eq!(loaded[1].body.as_str(), "event 1");
    }

    #[test]
    fn tool_details_survive_a_round_trip() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        store
            .insert_observation(&new_observation(
                session.id,
                EventKind::ToolUse,
                Some(ToolRef {
                    name: "Bash".to_owned(),
                    ok: Some(false),
                }),
                BoundedBody::truncating("cargo test", 1024),
                now(),
            ))
            .expect("insert");

        let loaded = store.observations(session.id).expect("load");
        let tool = loaded[0].tool.as_ref().expect("tool recorded");
        assert_eq!(tool.name, "Bash");
        assert_eq!(tool.ok, Some(false));
    }

    #[test]
    fn truncation_is_remembered_across_storage() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        store
            .insert_observation(&new_observation(
                session.id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating("x".repeat(100), 10),
                now(),
            ))
            .expect("insert");

        let loaded = store.observations(session.id).expect("load");
        assert!(loaded[0].body.is_truncated());
        assert_eq!(loaded[0].body.len(), 10);
    }

    /// Record a second session, as the SessionStart hook does before claiming.
    fn next_session(store: &Store, project: ProjectId, workspace: WorkspaceId) -> SessionId {
        let mut session = session_for(project, workspace);
        session.id = SessionId::derive(project, "agent-session-2");
        store.ensure_session(&session).expect("second session");
        session.id
    }

    #[test]
    fn a_handoff_can_only_be_claimed_once() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(project, session.id, "carry on", now()))
            .expect("record");

        let claimant = next_session(&store, project, workspace);
        let first = store.claim_handoff(project, claimant, now()).expect("claim");
        assert_eq!(first.as_deref(), Some("carry on"));

        let second = store.claim_handoff(project, claimant, now()).expect("claim");
        assert_eq!(second, None, "a handoff is single use");
    }

    #[test]
    fn a_claimant_must_be_a_real_session() {
        // The foreign key is the reason a handoff can never be attributed to a
        // session that was never recorded.
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");
        store
            .record_handoff(&new_handoff(project, session.id, "carry on", now()))
            .expect("record");

        assert!(
            store.claim_handoff(project, SessionId::new(), now()).is_err(),
            "an unknown claimant should be rejected"
        );
    }

    #[test]
    fn a_newer_handoff_replaces_one_nobody_read() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        store
            .record_handoff(&new_handoff(project, session.id, "older", now()))
            .expect("first");
        store
            .record_handoff(&new_handoff(project, session.id, "newer", now()))
            .expect("second");

        let claimant = next_session(&store, project, workspace);
        let claimed = store.claim_handoff(project, claimant, now()).expect("claim");
        assert_eq!(claimed.as_deref(), Some("newer"));
    }

    #[test]
    fn claiming_with_nothing_pending_returns_nothing() {
        let (_dir, store, project, workspace) = fixture();
        let claimant = next_session(&store, project, workspace);
        assert_eq!(
            store.claim_handoff(project, claimant, now()).expect("claim"),
            None
        );
    }

    #[test]
    fn closing_a_session_records_when_it_ended() {
        let (_dir, store, project, workspace) = fixture();
        let session = session_for(project, workspace);
        store.ensure_session(&session).expect("session");

        let ended: Timestamp = "2026-08-19T11:00:00Z".parse().expect("time");
        store.close_session(session.id, ended).expect("close");

        let loaded = store.load_session(session.id).expect("load").expect("found");
        assert!(!loaded.is_open());
        assert_eq!(loaded.ended_at, Some(ended));
    }

    #[test]
    fn session_ids_derived_from_one_agent_session_agree() {
        let (_dir, _store, project, _workspace) = fixture();
        assert_eq!(
            SessionId::derive(project, "abc-123"),
            SessionId::derive(project, "abc-123")
        );
        assert_ne!(
            SessionId::derive(project, "abc-123"),
            SessionId::derive(project, "abc-124")
        );
    }
}
