//! What happens to a hook payload between arriving and being durable.
//!
//! The whole capture path is here, in one function per boundary, because the
//! ordering matters and is easy to get subtly wrong: a session row has to exist
//! before an observation can reference it, and a handoff has to name a session
//! that was actually recorded.

use std::path::Path;
use std::sync::Arc;

use anamnesis_consolidate::{DigestSource, PREFERENCES_PAGE, SessionDigest, consolidate};
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
use anamnesis_store::{RawSpool, Store, SummarySource, new_handoff, new_observation, new_session};
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

/// What wrote a page, and which model was in play when it did or did not.
///
/// Carried together because either half alone misleads. "Counted" without the
/// model reads as "no model configured", which is a different fault; the model
/// name without the source reads as "this model wrote it", which is the claim
/// that was wrong in the first place.
#[derive(Debug, Clone, Copy)]
pub struct Provenance<'a> {
    /// Whether a model produced the page.
    pub source: SummarySource,
    /// The model that wrote it, or that was configured and did not answer.
    pub model: Option<&'a str>,
}

impl Provenance<'_> {
    /// The provenance of a page written with no model configured at all.
    #[must_use]
    pub fn counted() -> Self {
        Self {
            source: SummarySource::Counted,
            model: None,
        }
    }
}

/// Restate a consolidation's outcome as the store's spelling of it.
///
/// A free function rather than a `From` impl because both types belong to
/// other crates, and the two enums stay separate deliberately: one is what a
/// consolidation just did, the other is what a row remembers.
pub(crate) fn summary_source(source: DigestSource) -> SummarySource {
    match source {
        DigestSource::Model => SummarySource::Model,
        DigestSource::Counted => SummarySource::Counted,
    }
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

    commit(
        store,
        wiki,
        scope,
        &session,
        &digest,
        Provenance::counted(),
        embedder,
        now,
    )
    .map(Some)
}

/// Close a session, then ask a model what it was about.
///
/// Two steps, and the order is the whole point. [`finalize`] runs first and
/// touches no network: the page is written, the handoff recorded, the session
/// closed, in the time a file write and a git commit take. Only then is a
/// provider asked, and what it returns *replaces* a page that already exists.
///
/// It used to be one step, and the handler that called it wrote down what that
/// cost: the response goes out before the page does, so a server killed during
/// the model call lost the page and left the session open. That window was as
/// long as a model call. Now it is as long as a local write, and everything
/// past it is retriable — see [`crate::enrich`].
///
/// A model that refuses is therefore not an error here. The session is closed
/// and its page is real; the enrichment is the part that did not happen, it is
/// recorded as not having happened, and the pass in [`crate::enrich`] will try
/// again.
pub async fn finalize_and_enrich(
    store: &Arc<Store>,
    wiki: &Arc<Mutex<Wiki>>,
    scope: &ResolvedScope,
    session_id: SessionId,
    embedder: Option<Arc<dyn Embedder>>,
    now: Timestamp,
    llm: &LlmSettings,
) -> Result<Option<String>, WebError> {
    let written = {
        let store = store.clone();
        let wiki = wiki.clone();
        let scope = scope.clone();
        let embedder = embedder.clone();
        crate::off_runtime(move || {
            let held = wiki.lock();
            finalize(
                &store,
                &held,
                &scope,
                session_id,
                embedder
                    .as_ref()
                    .map(|embedder| embedder.as_ref() as &dyn Embed),
                now,
            )
        })
        .await?
    };

    // Nothing worth recording. `finalize` has closed the session and left no
    // page, and there is nothing for a model to improve on.
    let Some(page) = written else {
        return Ok(None);
    };

    // Failing here is not failing the call. The page is on disk and the
    // session is closed; what is lost is the reading of it, which is exactly
    // what the retry pass exists to collect.
    if let Err(error) =
        crate::enrich::enrich(store, wiki, scope, session_id, embedder, now, llm).await
    {
        tracing::error!(%error, %session_id, "could not enrich a session that closed cleanly");
    }

    Ok(Some(page))
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
///
/// Long in the same way `write_session_page` is: these are the things one
/// finished session consists of, and threading them through a struct would
/// only move the list somewhere the compiler checks less.
#[allow(clippy::too_many_arguments)]
fn commit(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session: &Session,
    digest: &SessionDigest,
    provenance: Provenance<'_>,
    embedder: Option<&dyn Embed>,
    now: Timestamp,
) -> Result<String, WebError> {
    let path = write_session_page(
        store,
        wiki,
        scope,
        session,
        digest,
        embedder,
        now,
        &format!("session: {}", digest.title),
    )?;
    // A no-op today, and here on purpose. The only digest that reaches this
    // function is the counted one, which never names a durable page — a
    // session closes before a model is asked anything, and the model's reading
    // arrives later through `recompile`. It is called anyway so that a path
    // which does hand this function a model's digest does not silently drop
    // what that model said the session left behind.
    let _ = write_notes(store, wiki, scope, session, digest, embedder, now);

    store.record_handoff(&new_handoff(
        scope.project_id,
        session.id,
        slot_for(scope, session),
        &digest.handoff,
        now,
    ))?;
    // Recorded before the close so that a session which is closed is never a
    // session whose page came from nowhere. The two are not in one transaction
    // and do not need to be: the failure this ordering avoids is a closed
    // session with no provenance, which `status` would read as "not summarised
    // yet" forever.
    store.record_summary(session.id, provenance.source, provenance.model)?;
    store.close_session(session.id, now)?;

    Ok(path)
}

/// What a recompile wrote.
///
/// The session's page was always the answer to "what did this do"; it stopped
/// being the whole answer when a reading could also leave durable pages
/// behind. A caller that reports one path while three files changed is a
/// caller that has to be checked against `git log` to be believed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recompiled {
    /// The session page, rewritten in place.
    pub page: String,
    /// Durable pages written beside it, if the reading named any.
    pub notes: Vec<String>,
}

/// Summarise a finished session again, and write its page over the old one.
///
/// Everything `commit` does about the page, and almost nothing it does about
/// the session. The two omissions are the point:
///
/// * **No handoff.** A handoff says what the *next* session should know, and
///   for a session that ended weeks ago the next session has already been and
///   gone. Leaving one in the waiting slot would hand somebody a briefing on
///   finished work — which is why `reindex` does not revive pending handoffs
///   either.
/// * **No close.** The session is closed already, at a time that is a fact
///   about it. Closing it again would stamp today on an afternoon in August,
///   and the page would then contradict the row it was rendered from.
///
/// What it does touch is the provenance, because that is a fact about the page
/// rather than about when the session ended, and this call has just replaced
/// the page it described.
///
/// The page path comes from `started_at`, which recompiling does not touch, so
/// the page is rewritten in place rather than joined by a second copy of the
/// same session.
#[allow(clippy::too_many_arguments)]
pub fn recompile(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session: &Session,
    digest: &SessionDigest,
    provenance: Provenance<'_>,
    embedder: Option<&dyn Embed>,
    now: Timestamp,
) -> Result<Recompiled, WebError> {
    let page = write_session_page(
        store,
        wiki,
        scope,
        session,
        digest,
        embedder,
        now,
        &format!("recompile: {}", digest.title),
    )?;
    let notes = write_notes(store, wiki, scope, session, digest, embedder, now);

    // The one thing a recompile does touch about the session. Its page has
    // just been replaced, so the old provenance describes a page that no
    // longer exists — and a recompile is most often run precisely to replace
    // counted pages once a model works, which is the moment `status` most
    // needs to stop reporting an outage that is over.
    store.record_summary(session.id, provenance.source, provenance.model)?;

    Ok(Recompiled { page, notes })
}

/// Render a digest to a session's page, and put that page in the index.
///
/// Shared by the live path and by `recompile` deliberately. What belongs in
/// the index when a page is written has been wrong twice here, both times
/// because a second writer built a subset of it; a page written by one path
/// and the same page written by the other have to end up indexed the same way.
#[allow(clippy::too_many_arguments)]
fn write_session_page(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session: &Session,
    digest: &SessionDigest,
    embedder: Option<&dyn Embed>,
    now: Timestamp,
    message: &str,
) -> Result<String, WebError> {
    let path = session_page_path(&session.started_at, session.id)?;
    let mut frontmatter = Frontmatter::new(&digest.title, digest.entities.clone())?;
    frontmatter.tier = Tier::Episodic;
    // Written into the markdown rather than only into the index, because the
    // index is rebuilt from the markdown: a page that did not say which session
    // wrote it would forget on the next `reindex`.
    frontmatter.session = Some(session.id);

    let mut page = Page::new(
        scope.project_id,
        path.clone(),
        frontmatter,
        attributed(&digest.body, session),
    );
    let commit = wiki.write_page(&scope.scope, &page, message)?;
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

    Ok(path.as_str().to_owned())
}

/// Write the durable pages a digest named, in one commit.
///
/// Returns nothing, and cannot fail the caller, which is the whole shape of
/// this function. The rule the model path is built on — a model may improve
/// what a session leaves behind and may never be the reason there is nothing —
/// reaches its last mile here: the session's own page is already written and
/// its handoff is about to be recorded, and neither may be lost because a
/// durable page beside them could not be. A failure is logged and the session
/// closes, and the caller is told nothing was written rather than told why —
/// there is nothing it could do differently.
///
/// One commit for the batch, separate from the session page's. Separate
/// because they are two writes with two subjects and the `session:` and
/// `recompile:` messages are a shape other things read; one commit for the
/// batch because a consolidation deciding a session left a decision, a gotcha
/// and a procedure behind is a single decision about this project's memory.
fn write_notes(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session: &Session,
    digest: &SessionDigest,
    embedder: Option<&dyn Embed>,
    now: Timestamp,
) -> Vec<String> {
    if digest.notes.is_empty() {
        return Vec::new();
    }
    match try_write_notes(store, wiki, scope, session, digest, embedder, now) {
        Ok(written) => written,
        Err(error) => {
            tracing::error!(
                %error,
                session_id = %session.id,
                "could not write the durable pages a session left; its own page stands"
            );
            Vec::new()
        }
    }
}

/// The body of [`write_notes`], with the errors it is not allowed to raise.
fn try_write_notes(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    session: &Session,
    digest: &SessionDigest,
    embedder: Option<&dyn Embed>,
    now: Timestamp,
) -> Result<Vec<String>, WebError> {
    // What this session has already written, which is the only thing it may
    // write over. A recompile has to be able to replace the notes its earlier
    // run left, or recompiling would either duplicate them under new names or
    // do nothing at all. Anything else at that path belongs to somebody else —
    // a person who wrote the page by hand, or another session that got there
    // first — and a summary of one session does not get to overwrite it.
    let mine = store.pages_from_session(session.id)?;

    let mut pages = Vec::with_capacity(digest.notes.len());
    for note in &digest.notes {
        if wiki.exists(&scope.scope, &note.path) && !mine.contains(&note.path) {
            tracing::info!(
                path = %note.path.as_str(),
                session_id = %session.id,
                "a page already stands there; the note was not written over it"
            );
            continue;
        }

        let mut frontmatter = Frontmatter::new(&note.title, Vec::new())?;
        // The tier is what stops a sweep from reading these as one session's
        // leftovers, and the session id is what a later recompile reads to
        // know which of them are its own.
        frontmatter.tier = note.kind.tier();
        frontmatter.session = Some(session.id);

        pages.push(Page::new(
            scope.project_id,
            note.path.clone(),
            frontmatter,
            note.body.clone(),
        ));
    }

    let Some(commit) = wiki.write_pages(&scope.scope, &pages, &notes_message(digest, &pages))?
    else {
        return Ok(Vec::new());
    };

    // Indexed here rather than left to `reindex`, for the reason
    // `write_session_page` gives: the index the live path builds and the index
    // a rebuild reproduces have to be the same index.
    let mut written = Vec::with_capacity(pages.len());
    for mut page in pages {
        page.git_commit = Some(commit.clone());
        store.index_page(
            scope.project_id,
            &page,
            &anamnesis_wiki::extract_links(&page.body),
            embedder,
            now,
        )?;
        written.push(page.path.as_str().to_owned());
    }

    Ok(written)
}

/// The commit message for a batch of notes.
///
/// The subject says how many and which session they came from; the body names
/// every page, following the sweep's message, because a commit that touches
/// three files under three namespaces is otherwise unreadable in a log.
fn notes_message(digest: &SessionDigest, pages: &[Page]) -> String {
    let plural = if pages.len() == 1 { "page" } else { "pages" };
    let mut message = format!(
        "notes: {} durable {plural} from {}\n",
        pages.len(),
        digest.title
    );
    for page in pages {
        message.push_str(&format!("\n- {}", page.path.as_str()));
    }
    message.push('\n');
    message
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
pub fn read_preferences(wiki: &Wiki, scope: &ResolvedScope) -> Option<String> {
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
pub(crate) fn slot_for(scope: &ResolvedScope, session: &Session) -> Slot {
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
pub fn session_page_path(started_at: &Timestamp, id: SessionId) -> Result<PagePath, WebError> {
    let stamp = started_at.to_string();
    let date = stamp.split('T').next().unwrap_or("undated");
    let short: String = id.to_string().chars().take(8).collect();
    Ok(PagePath::parse(&format!("sessions/{date}-{short}.md"))?)
}

/// Resolve the scope for a working directory reported by a hook.
pub(crate) fn scope_for(cwd: &Path) -> Result<ResolvedScope, WebError> {
    resolve_scope(cwd).map_err(WebError::from)
}
