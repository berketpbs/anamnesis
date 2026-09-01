//! What happens to a hook payload between arriving and being durable.
//!
//! The whole capture path is here, in one function per boundary, because the
//! ordering matters and is easy to get subtly wrong: a session row has to exist
//! before an observation can reference it, and a handoff has to name a session
//! that was actually recorded.

use std::path::Path;
use std::sync::Arc;

use anamnesis_consolidate::{PREFERENCES_PAGE, SessionDigest, consolidate, consolidate_with_llm};
use anamnesis_core::capture::CaptureFilter;
use anamnesis_core::embedding::Embed;
use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::{EventKind, Observation};
use anamnesis_core::page::{Frontmatter, Page, PagePath, Tier};
use anamnesis_core::scope::{OperatorName, ResolvedScope, resolve_scope};
use anamnesis_core::session::{AgentKind, Session};
use anamnesis_hooks::ParsedHook;
use anamnesis_llm::Embedder;
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
    embedder: Option<&dyn Embed>,
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

    let page = finalize(store, wiki, &scope, session_id, embedder, now)?;
    Ok(Ingested {
        session_id,
        consolidated: true,
        page,
    })
}

/// What recording this event would do, reported without doing any of it.
///
/// Every field here is something the capture path decides on its way to a
/// write: which project the working directory resolves to, which session the
/// harness's identifier derives, whether `[capture] ignore_paths` would drop
/// the event, what redaction caught. A probe that only asked "is the server
/// up" would answer the easy half of the question — the failures worth
/// finding are the ones where the server is up and the event still goes
/// nowhere.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeReport {
    /// Whether this event would be stored, had it not been a probe.
    pub would_record: bool,
    /// The harness the payload was read as.
    pub agent: String,
    /// The lifecycle event it was classified as.
    pub event: String,
    /// Workspace the working directory resolved to.
    pub workspace: String,
    /// Project the working directory resolved to.
    pub project: String,
    /// The session identifier this payload derives.
    pub session: String,
    /// Whether that session is already in the index.
    pub session_known: bool,
    /// The first path `[capture] ignore_paths` would drop the event for.
    pub excluded: Option<String>,
    /// Redaction rules that fired on the payload.
    pub redactions: Vec<String>,
    /// Whether a handoff is waiting in the slot this session would read.
    ///
    /// Peeked, never claimed. A handoff is single-use, and a diagnostic that
    /// consumed the note the next session was owed would be the most
    /// expensive way to ask whether memory is working.
    pub handoff_waiting: bool,
    /// How the server would summarise this session.
    pub consolidation: Consolidation,
}

/// What the server would compile a session into.
///
/// Reported because it is the difference between memory that holds knowledge
/// and memory that holds counts, and nothing else a probe can see says which
/// one is running: capture works identically either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Consolidation {
    /// A model writes the summary.
    Model,
    /// The summary is counted from what the session did.
    Counted,
}

/// Answer what [`record`] would do with this event, and write nothing.
///
/// Everything up to the first write is real: the payload is parsed and
/// redacted by the same code, the scope is resolved from the same working
/// directory, the session identifier is derived by the same rule. What is
/// skipped is every write — the project row, the session, the observation,
/// the spool — and the handoff is peeked rather than claimed.
pub fn probe(
    store: &Store,
    hook: &ParsedHook,
    now: Timestamp,
    operator: Option<&OperatorName>,
    model: bool,
) -> Result<ProbeReport, WebError> {
    let cwd = hook
        .cwd
        .as_deref()
        .ok_or_else(|| WebError::BadRequest("hook payload has no cwd".to_owned()))?;
    let scope = scope_for(cwd)?;

    let session_id = SessionId::derive(scope.project_id, &hook.agent_session_id);
    // Built exactly as `record` builds it, and then not stored. Deriving the
    // slot from anything else would let the probe report a waiting handoff
    // that the real path would not find.
    let session = new_session(
        session_id,
        scope.project_id,
        scope.workspace_id,
        hook.agent.clone(),
        cwd.to_path_buf(),
        now,
        None,
    )
    .with_operator(operator.cloned());

    let excluded = excluded_path(&scope, hook);
    let slot = slot_for(&scope, &session);

    Ok(ProbeReport {
        would_record: excluded.is_none(),
        agent: hook.agent.to_string(),
        event: hook.kind.as_str().to_owned(),
        workspace: scope.scope.workspace.to_string(),
        project: scope.scope.project.to_string(),
        session: session_id.to_string(),
        session_known: store.load_session(session_id)?.is_some(),
        excluded,
        redactions: hook
            .redactions
            .iter()
            .map(|rule| (*rule).to_owned())
            .collect(),
        handoff_waiting: store.peek_handoff(scope.project_id, &slot)?.is_some(),
        consolidation: if model {
            Consolidation::Model
        } else {
            Consolidation::Counted
        },
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

    // An agent that went quiet long enough to be summarised and then carried
    // on gets its session back. Without this the reaper would be destructive:
    // everything after the summary would land in a closed session that nothing
    // ever reads again. Ending it a second time rewrites the same page and
    // supersedes its handoff, so the only cost is the second pass.
    store.resume_session(session_id)?;

    // The session as it is stored, not as it was just built. They differ for
    // every event after the first: `new_session` stamps `started_at` with
    // *now*, and `ensure_session` keeps the row that is already there. Only
    // the stored one can say when the session began — and the transcript's
    // path is derived from that, so writing against the fresh one filed a
    // session that ran past midnight under two dates, with the second file
    // claiming a start time hours after the truth and unreachable by every
    // command that looks a transcript up by name.
    //
    // One indexed lookup by primary key, on a path that already runs several
    // statements. It buys a transcript per session rather than per calendar
    // day it touched.
    let session = store.load_session(session_id)?.unwrap_or(session);

    let mut observation = new_observation(
        session_id,
        hook.kind,
        hook.tool.clone(),
        hook.body.clone(),
        now,
    );
    // The sender's name for this delivery, where it gave one. Minting a fresh
    // identifier instead would make the same event offered twice into two
    // events, and a session that counts one prompt twice is summarised wrongly
    // by every path that reads it.
    if let Some(delivery) = hook.delivery {
        observation.id = delivery;
    }
    let first_arrival = store.insert_observation(&observation)?;

    // The index is the authority for this request; the spool is the durable
    // copy behind it. A spool that cannot be written is logged and stepped
    // over rather than failing the event: losing the durable copy is bad,
    // losing the event itself because a disk filled up is worse.
    //
    // Skipped outright when this event was already recorded: the spool is
    // append-only, so a second line for it could never be taken back, and the
    // transcript is the copy that outlives the index.
    if first_arrival
        && let Some(raw) = raw
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
    embedder: Option<&dyn Embed>,
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

    commit(store, wiki, scope, &session, &digest, embedder, now).map(Some)
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
    store: &Arc<Store>,
    wiki: &Arc<Mutex<Wiki>>,
    scope: &ResolvedScope,
    session_id: SessionId,
    embedder: Option<Arc<dyn Embedder>>,
    now: Timestamp,
    llm: &LlmSettings,
) -> Result<Option<String>, WebError> {
    // Three phases, and the middle one is the only one that belongs on the
    // runtime. Reading a session out of SQLite and reading a preferences page
    // off disk are blocking calls; so is writing the page, committing it to
    // git, and embedding it. Only the model is a network wait.
    let loaded = {
        let store = store.clone();
        let wiki = wiki.clone();
        let scope = scope.clone();
        crate::off_runtime(move || -> Result<_, WebError> {
            let Some((session, observations)) = prepare(&store, session_id, now)? else {
                return Ok(None);
            };
            let preferences = {
                let wiki = wiki.lock();
                read_preferences(&wiki, &scope)
            };
            Ok(Some((session, observations, preferences)))
        })
        .await?
    };
    let Some((session, observations, preferences)) = loaded else {
        return Ok(None);
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
        let store = store.clone();
        return crate::off_runtime(move || -> Result<_, WebError> {
            store.close_session(session_id, now)?;
            Ok(None)
        })
        .await;
    };

    let store = store.clone();
    let wiki = wiki.clone();
    let scope = scope.clone();
    crate::off_runtime(move || {
        let held = wiki.lock();
        commit(
            &store,
            &held,
            &scope,
            &session,
            &digest,
            embedder
                .as_ref()
                .map(|embedder| embedder.as_ref() as &dyn Embed),
            now,
        )
        .map(Some)
    })
    .await
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
    embedder: Option<&dyn Embed>,
    now: Timestamp,
) -> Result<String, WebError> {
    let path = session_page_path(&session.started_at, session.id)?;
    let mut frontmatter = Frontmatter::new(&digest.title, digest.entities.clone())?;
    frontmatter.tier = Tier::Episodic;

    let mut page = Page::new(
        scope.project_id,
        path.clone(),
        frontmatter,
        attributed(&digest.body, session),
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
        embedder,
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

/// The session's own account of who ran it.
///
/// Written here rather than in either renderer, because whose session this was
/// is a fact about the session and not about how its summary was written: the
/// deterministic path and the model path must not be able to disagree about
/// it. The model is never told the name to begin with — an operator's identity
/// is not something to hand a provider along with their transcript — so a
/// summary it wrote could not carry the attribution even if asked.
///
/// A server with no tokens has no name to write, and stamping "unknown" on
/// every page of every single-person install would be noise standing in for a
/// fact nobody was missing. On a shared server the line is the difference
/// between a wiki of sessions and a wiki of *somebody's* sessions.
fn attributed(body: &str, session: &Session) -> String {
    match &session.operator {
        None => body.to_owned(),
        Some(operator) => format!("{}\n\nRecorded by {operator}.\n", body.trim_end()),
    }
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
pub(crate) fn scope_for(cwd: &Path) -> Result<ResolvedScope, WebError> {
    resolve_scope(cwd).map_err(WebError::from)
}
