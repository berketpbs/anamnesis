//! What happens to a hook payload between arriving and being durable.
//!
//! The whole capture path is here, in one function per boundary, because the
//! ordering matters and is easy to get subtly wrong: a session row has to exist
//! before an observation can reference it, and a handoff has to name a session
//! that was actually recorded.

use std::path::Path;

use anamnesis_consolidate::consolidate;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::EventKind;
use anamnesis_core::page::{Frontmatter, Page, PagePath, Tier};
use anamnesis_core::scope::{ResolvedScope, resolve_scope};
use anamnesis_core::session::AgentKind;
use anamnesis_hooks::ParsedHook;
use anamnesis_store::{Store, new_handoff, new_observation, new_session};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

use crate::WebError;

/// Outcome of ingesting one hook payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// Session the event was attributed to.
    pub session_id: SessionId,
    /// Whether this event closed the session and produced a page.
    pub consolidated: bool,
    /// Page written, when one was.
    pub page: Option<String>,
}

/// Record one hook event, consolidating the session if it just ended.
pub fn ingest(
    store: &Store,
    wiki: &Wiki,
    hook: &ParsedHook,
    now: Timestamp,
) -> Result<Ingested, WebError> {
    let cwd = hook
        .cwd
        .as_deref()
        .ok_or_else(|| WebError::BadRequest("hook payload has no cwd".to_owned()))?;
    let scope = scope_for(cwd)?;

    store.upsert_project(&scope, now)?;

    let session_id = SessionId::derive(scope.project_id, &hook.agent_session_id);
    store.ensure_session(&new_session(
        session_id,
        scope.project_id,
        scope.workspace_id,
        hook.agent.clone(),
        cwd.to_path_buf(),
        now,
    ))?;

    store.insert_observation(&new_observation(
        session_id,
        hook.kind,
        hook.tool.clone(),
        hook.body.clone(),
        now,
    ))?;

    if hook.kind != EventKind::SessionEnd {
        return Ok(Ingested {
            session_id,
            consolidated: false,
            page: None,
        });
    }

    let page = finalize(store, wiki, &scope, session_id, now)?;
    Ok(Ingested {
        session_id,
        consolidated: true,
        page,
    })
}

/// Close a session: summarise it, write the page, leave a handoff.
///
/// Returns the page path, or `None` when the session had nothing in it worth
/// recording.
pub fn finalize(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session_id: SessionId,
    now: Timestamp,
) -> Result<Option<String>, WebError> {
    let Some(mut session) = store.load_session(session_id)? else {
        return Ok(None);
    };
    session.ended_at = Some(now);

    let observations = store.observations(session_id)?;
    let Some(digest) = consolidate(&session, &observations) else {
        // Nothing but boundaries. Close it and leave no trace: a wiki full of
        // empty session stubs makes every later search worse.
        store.close_session(session_id, now)?;
        return Ok(None);
    };

    let path = session_page_path(&session.started_at, session_id)?;
    let mut frontmatter = Frontmatter::new(&digest.title, Vec::new())?;
    frontmatter.tier = Tier::Episodic;

    let mut page = Page::new(scope.project_id, path.clone(), frontmatter, digest.body);
    let commit = wiki.write_page(&scope.scope, &page, &format!("session: {}", digest.title))?;
    page.git_commit = Some(commit);

    store.upsert_page(&page, now)?;
    store.record_handoff(&new_handoff(
        scope.project_id,
        session_id,
        &digest.handoff,
        now,
    ))?;
    store.close_session(session_id, now)?;

    Ok(Some(path.as_str().to_owned()))
}

/// Hand the pending handoff, if any, to a starting session.
pub fn claim_handoff(
    store: &Store,
    cwd: &Path,
    agent: &AgentKind,
    agent_session_id: &str,
    now: Timestamp,
) -> Result<Option<String>, WebError> {
    let scope = scope_for(cwd)?;
    store.upsert_project(&scope, now)?;

    // The claimant has to be a recorded session before it can be named as the
    // recipient; the schema enforces that, and this is where it is satisfied.
    let session_id = SessionId::derive(scope.project_id, agent_session_id);
    store.ensure_session(&new_session(
        session_id,
        scope.project_id,
        scope.workspace_id,
        agent.clone(),
        cwd.to_path_buf(),
        now,
    ))?;

    Ok(store.claim_handoff(scope.project_id, session_id, now)?)
}

/// Where a session's page lives: `sessions/<date>-<short id>.md`.
///
/// The identifier is part of the filename because two sessions on one day are
/// ordinary, and the date is first because sorting by name should sort by time.
fn session_page_path(started_at: &Timestamp, id: SessionId) -> Result<PagePath, WebError> {
    let stamp = started_at.to_string();
    let date = stamp.split('T').next().unwrap_or("undated");
    let short: String = id.to_string().chars().take(8).collect();
    Ok(PagePath::parse(&format!("sessions/{date}-{short}.md"))?)
}

/// Resolve the scope for a working directory reported by a hook.
fn scope_for(cwd: &Path) -> Result<ResolvedScope, WebError> {
    resolve_scope(cwd).map_err(WebError::from)
}
