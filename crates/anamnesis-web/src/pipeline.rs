//! What happens to a hook payload between arriving and being durable.
//!
//! The whole capture path is here, in one function per boundary, because the
//! ordering matters and is easy to get subtly wrong: a session row has to exist
//! before an observation can reference it, and a handoff has to name a session
//! that was actually recorded.

use std::path::Path;

use anamnesis_consolidate::{PREFERENCES_PAGE, SessionDigest, consolidate, consolidate_with_llm};
use anamnesis_core::capture::CaptureFilter;
use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::{EventKind, Observation};
use anamnesis_core::page::{Frontmatter, Page, PagePath, Tier};
use anamnesis_core::scope::{OperatorName, ResolvedScope, resolve_scope};
use anamnesis_core::session::{AgentKind, Session};
use anamnesis_hooks::ParsedHook;
use anamnesis_store::{RawSpool, Store, new_handoff, new_observation, new_session};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;
use parking_lot::Mutex;

use crate::{LlmSettings, WebError};

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
    raw: Option<&RawSpool>,
    hook: &ParsedHook,
    now: Timestamp,
    operator: Option<&OperatorName>,
) -> Result<Ingested, WebError> {
    let (scope, session_id) = record(store, raw, hook, now, operator)?;

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

/// Make one event durable, and stop there.
///
/// Separated from [`ingest`] because consolidation may want to take its time —
/// a model call is seconds, and the hook that delivered this event is a
/// subprocess of somebody's editor with a one-second patience. Recording is
/// the part that must not be deferred; deciding what the session meant is the
/// part that must not block.
pub fn record(
    store: &Store,
    raw: Option<&RawSpool>,
    hook: &ParsedHook,
    now: Timestamp,
    operator: Option<&OperatorName>,
) -> Result<(ResolvedScope, SessionId), WebError> {
    let cwd = hook
        .cwd
        .as_deref()
        .ok_or_else(|| WebError::BadRequest("hook payload has no cwd".to_owned()))?;
    let scope = scope_for(cwd)?;

    store.upsert_project(&scope, now)?;

    let session_id = SessionId::derive(scope.project_id, &hook.agent_session_id);
    let session = new_session(
        session_id,
        scope.project_id,
        scope.workspace_id,
        hook.agent.clone(),
        cwd.to_path_buf(),
        now,
        // The hook capture path knows nothing about workstreams — that is an
        // MCP-only concept for now. Every session it records shares the
        // project-wide handoff slot, exactly as before workstreams existed.
        None,
    )
    // Recorded whether or not this project keys slots by operator: who ran a
    // session is worth knowing on its own, and only the *slot* is a setting.
    .with_operator(operator.cloned());
    store.ensure_session(&session)?;

    // An excluded file is excluded from the moment it arrives: the observation
    // is never built, so nothing about it reaches the index, the spool, or a
    // later summary. The session row stays, because a session whose middle was
    // filtered still started and still has to end.
    if let Some(excluded) = excluded_path(&scope, hook) {
        tracing::debug!(
            %session_id,
            path = %excluded,
            "dropping event: path is excluded by [capture] ignore_paths"
        );
        return Ok((scope, session_id));
    }

    let observation = new_observation(
        session_id,
        hook.kind,
        hook.tool.clone(),
        hook.body.clone(),
        now,
    );
    store.insert_observation(&observation)?;

    // The index is the authority for this request; the spool is the durable
    // copy behind it. A spool that cannot be written is logged and stepped
    // over rather than failing the event: losing the durable copy is bad,
    // losing the event itself because a disk filled up is worse.
    if let Some(raw) = raw
        && let Err(error) = raw.append(&scope.scope, &session, &observation)
    {
        tracing::error!(%error, %session_id, "could not spool observation");
    }

    Ok((scope, session_id))
}

/// The first path in this event the project has asked never to capture.
///
/// A malformed pattern excludes nothing and says so once, rather than failing
/// the event: a typo in a marker file must not stop a session being recorded,
/// and the alternative — treating an uncompilable pattern as "exclude
/// everything" — would silently empty someone's memory.
fn excluded_path(scope: &ResolvedScope, hook: &ParsedHook) -> Option<String> {
    if hook.paths.is_empty() || scope.capture.ignore_paths.is_empty() {
        return None;
    }

    let filter = match CaptureFilter::compile(&scope.capture, &scope.root) {
        Ok(filter) => filter,
        Err(error) => {
            tracing::warn!(%error, "ignoring unusable [capture] ignore_paths");
            return None;
        }
    };

    filter
        .first_excluded(hook.paths.iter().map(String::as_str))
        .map(str::to_owned)
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
    let Some((session, observations)) = prepare(store, session_id, now)? else {
        return Ok(None);
    };
    let Some(digest) = consolidate(&session, &observations) else {
        // Nothing but boundaries. Close it and leave no trace: a wiki full of
        // empty session stubs makes every later search worse.
        store.close_session(session_id, now)?;
        return Ok(None);
    };

    commit(store, wiki, scope, &session, &digest, now).map(Some)
}

/// Close a session, asking a model what it was about.
///
/// The same three steps as [`finalize`] with the middle one replaced. The
/// shape matters more than it looks: the model call happens between the two
/// locked sections, holding neither the wiki nor a database transaction, so a
/// slow or hanging provider delays exactly one session's page and blocks
/// nothing else. Holding the wiki mutex across that await would serialise
/// every other session ending in the same minute behind it.
pub async fn finalize_with_llm(
    store: &Store,
    wiki: &Mutex<Wiki>,
    scope: &ResolvedScope,
    session_id: SessionId,
    now: Timestamp,
    llm: &LlmSettings,
) -> Result<Option<String>, WebError> {
    let Some((session, observations)) = prepare(store, session_id, now)? else {
        return Ok(None);
    };

    let preferences = {
        let wiki = wiki.lock();
        read_preferences(&wiki, scope)
    };

    let digest = consolidate_with_llm(
        llm.provider.as_ref(),
        &session,
        &observations,
        preferences.as_deref(),
        llm.max_input_tokens,
        llm.max_output_tokens,
    )
    .await;

    let Some(digest) = digest else {
        store.close_session(session_id, now)?;
        return Ok(None);
    };

    let wiki = wiki.lock();
    commit(store, &wiki, scope, &session, &digest, now).map(Some)
}

/// Load what a finished session consists of.
///
/// `ended_at` is set on the returned copy rather than written back: nothing is
/// committed until the page is, so a consolidation that fails leaves the
/// session open and retryable rather than closed and empty.
fn prepare(
    store: &Store,
    session_id: SessionId,
    now: Timestamp,
) -> Result<Option<(Session, Vec<Observation>)>, WebError> {
    let Some(mut session) = store.load_session(session_id)? else {
        return Ok(None);
    };
    session.ended_at = Some(now);
    let observations = store.observations(session_id)?;
    Ok(Some((session, observations)))
}

/// Write the page, record the handoff, close the session.
fn commit(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session: &Session,
    digest: &SessionDigest,
    now: Timestamp,
) -> Result<String, WebError> {
    let path = session_page_path(&session.started_at, session.id)?;
    let mut frontmatter = Frontmatter::new(&digest.title, digest.entities.clone())?;
    frontmatter.tier = Tier::Episodic;

    let mut page = Page::new(
        scope.project_id,
        path.clone(),
        frontmatter,
        digest.body.clone(),
    );
    let commit = wiki.write_page(&scope.scope, &page, &format!("session: {}", digest.title))?;
    page.git_commit = Some(commit);

    // Everything a rebuild would put in the index, in one call. Leaving any of
    // it to `reindex` would mean the index the live path builds and the index a
    // rebuild reproduces are not the same index — which they were not, twice.
    store.index_page(
        scope.project_id,
        &page,
        &anamnesis_wiki::extract_links(&page.body),
        None,
        now,
    )?;

    store.record_handoff(&new_handoff(
        scope.project_id,
        session.id,
        slot_for(scope, session),
        &digest.handoff,
        now,
    ))?;
    store.close_session(session.id, now)?;

    Ok(path.as_str().to_owned())
}

/// Read the project's consolidation preferences, if it has written any.
///
/// Absent is the normal case and not worth a log line. Unreadable is treated
/// the same way: a preferences page is a nicety, and failing a session's
/// consolidation because someone left a directory where a file was expected
/// would trade something valuable for something optional.
fn read_preferences(wiki: &Wiki, scope: &ResolvedScope) -> Option<String> {
    let path = PagePath::parse(PREFERENCES_PAGE).ok()?;
    std::fs::read_to_string(wiki.locate(&scope.scope, &path)).ok()
}

/// Hand the pending handoff, if any, to a starting session.
pub fn claim_handoff(
    store: &Store,
    cwd: &Path,
    agent: &AgentKind,
    agent_session_id: &str,
    now: Timestamp,
    operator: Option<&OperatorName>,
) -> Result<Option<String>, WebError> {
    let scope = scope_for(cwd)?;
    store.upsert_project(&scope, now)?;

    // The claimant has to be a recorded session before it can be named as the
    // recipient; the schema enforces that, and this is where it is satisfied.
    let session_id = SessionId::derive(scope.project_id, agent_session_id);
    let session = new_session(
        session_id,
        scope.project_id,
        scope.workspace_id,
        agent.clone(),
        cwd.to_path_buf(),
        now,
        None,
    )
    .with_operator(operator.cloned());
    store.ensure_session(&session)?;

    // Hooks have no concept of a workstream yet, so the workstream half of the
    // slot is always the shared one.
    let slot = slot_for(&scope, &session);
    Ok(store.claim_handoff(scope.project_id, session_id, &slot, now)?)
}

/// The slot a session writes its handoff into, and reads one from.
///
/// The workstream half is the session's own. The operator half is the
/// session's too, but only where the project asked for per-operator slots.
/// Keying by an operator a project never asked to separate would hide the
/// waiting note the first time somebody presented a different token — and the
/// symptom, an empty handoff, is the one this system is least able to explain.
fn slot_for(scope: &ResolvedScope, session: &Session) -> Slot {
    let operator = if scope.slots.per_user {
        session.operator.clone()
    } else {
        None
    };
    Slot::for_workstream(session.workstream_id).for_operator(operator)
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
