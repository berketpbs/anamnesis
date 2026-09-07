//! Asking a model what a session was about, after the session has closed.
//!
//! Consolidation used to be one step: `SessionEnd` arrived, a model was asked,
//! and whatever came back became the page. The cost of that was written down in
//! the handler that ran it — the response goes out first and the page is
//! written behind it, so "a server killed in the next few seconds loses the
//! page, and the session stays open rather than closing with nothing in it."
//! The window that sentence describes is the length of a model call.
//!
//! Splitting it in two closes the window rather than narrowing it. The session
//! ends and [`crate::pipeline::finalize`] writes the counted page immediately:
//! no network, no provider, a few milliseconds. Only then is a model asked, and
//! everything it contributes is a *replacement* for something that already
//! exists. A crash now costs the enrichment, which the pass below will
//! reattempt, instead of costing the page and leaving the session open.
//!
//! That is also what makes an outage survivable. Yesterday's provenance columns
//! record whether a model wrote a page or the counts did; a session that reads
//! `counted` is a session worth asking about again, and [`run_enricher`] is the
//! thing that asks.

use std::sync::Arc;

use anamnesis_consolidate::consolidate_with_source;
use anamnesis_core::embedding::Embed;
use anamnesis_core::ids::SessionId;
use anamnesis_core::scope::{ResolvedScope, resolve_scope};
use anamnesis_llm::Embedder;
use anamnesis_store::{Store, SummarySource, new_handoff};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;
use parking_lot::Mutex;

use crate::improve::TICK;
use crate::pipeline::{Provenance, read_preferences, recompile, slot_for, summary_source};
use crate::{AppState, LlmSettings, WebError};

/// How many sessions one pass of [`run_enricher`] will ask about.
///
/// Small because the thing on the other end has a rate limit and, on the free
/// tiers this runs against, a daily one. A backlog that drains over several
/// passes is the intended behaviour, not a compromise: the sessions are closed
/// and their pages exist, so nothing is waiting on this.
const BATCH: usize = 3;

/// What asking a model about one session came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enriched {
    /// A model wrote the page, and this is where it went.
    Wrote(String),
    /// The model was asked and did not answer with a page. The counted one it
    /// already had stands.
    Counted,
    /// There was nothing to ask about — no session, or nothing in it.
    Nothing,
}

/// Ask a model to rewrite one closed session's page.
///
/// The session is expected to be closed already and its page written. Nothing
/// here closes a session, stamps an end time, or leaves a handoff where there
/// was none: this runs after all of that, possibly days after, and every one of
/// those would be a lie about when the work happened.
///
/// The one thing it does replace is a handoff that is *still waiting*. The note
/// written at session end says what the counts saw; the model's says what the
/// session meant, and the next agent would rather have the second. But only
/// while nobody has read the first — see
/// [`Store::supersede_pending_handoff`](anamnesis_store::Store::supersede_pending_handoff).
pub async fn enrich(
    store: &Arc<Store>,
    wiki: &Arc<Mutex<Wiki>>,
    scope: &ResolvedScope,
    session_id: SessionId,
    embedder: Option<Arc<dyn Embedder>>,
    now: Timestamp,
    llm: &LlmSettings,
) -> Result<Enriched, WebError> {
    // Same three phases as the path this was split out of, and the same reason:
    // only the model call belongs on the runtime, and it holds neither the wiki
    // nor a transaction while it waits.
    let loaded = {
        let store = store.clone();
        let wiki = wiki.clone();
        let scope = scope.clone();
        crate::off_runtime(move || -> Result<_, WebError> {
            let Some(session) = store.load_session(session_id)? else {
                return Ok(None);
            };
            let observations = store.observations(session_id)?;
            if observations.is_empty() {
                return Ok(None);
            }
            let preferences = {
                let wiki = wiki.lock();
                read_preferences(&wiki, &scope)
            };
            Ok(Some((session, observations, preferences)))
        })
        .await?
    };
    let Some((session, observations, preferences)) = loaded else {
        return Ok(Enriched::Nothing);
    };

    let compiled = consolidate_with_source(
        llm.provider.as_ref(),
        &session,
        &observations,
        preferences.as_deref(),
        llm.max_input_tokens,
        llm.max_output_tokens,
    )
    .await;

    let model = llm.provider.model().to_owned();
    let Some((digest, source)) = compiled else {
        return Ok(Enriched::Nothing);
    };

    // A counted reply is not written. The page it would produce is the page
    // that is already there, so writing it would cost a commit, renew the
    // decay clock, and change nothing a reader could see. Recording the
    // provenance is still worth it: it names the model that did not answer,
    // and leaves the session in the queue for the next pass.
    if summary_source(source) == SummarySource::Counted {
        let store = store.clone();
        let model = model.clone();
        crate::off_runtime(move || -> Result<_, WebError> {
            store.record_summary(session_id, SummarySource::Counted, Some(&model))?;
            Ok(())
        })
        .await?;
        return Ok(Enriched::Counted);
    }

    let store = store.clone();
    let wiki = wiki.clone();
    let scope = scope.clone();
    crate::off_runtime(move || -> Result<_, WebError> {
        let page = {
            let held = wiki.lock();
            recompile(
                &store,
                &held,
                &scope,
                &session,
                &digest,
                Provenance {
                    source: SummarySource::Model,
                    model: Some(&model),
                },
                embedder
                    .as_ref()
                    .map(|embedder| embedder.as_ref() as &dyn Embed),
                now,
            )?
        };

        let handoff = new_handoff(
            scope.project_id,
            session.id,
            slot_for(&scope, &session),
            &digest.handoff,
            now,
        );
        match store.supersede_pending_handoff(&handoff) {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                session = %session_id,
                "the handoff this session left had already been claimed; left it alone"
            ),
            // The page is written and the session is closed; a handoff that
            // could not be replaced is worth a line and nothing more.
            Err(error) => tracing::warn!(
                %error,
                session = %session_id,
                "could not replace the handoff a session left"
            ),
        }

        Ok(Enriched::Wrote(page))
    })
    .await
}

/// Ask again about the sessions a model never answered for.
///
/// One pass, over a handful of sessions at a time. Returns how many pages it
/// wrote, which is what the tests assert on and what the log line reports.
pub async fn sweep_awaiting(state: &AppState, now: Timestamp) -> usize {
    let Some(settings) = state.llm.clone() else {
        return 0;
    };

    let waiting = match state.store.sessions_awaiting_enrichment(BATCH) {
        Ok(waiting) => waiting,
        Err(error) => {
            tracing::error!(%error, "could not list the sessions awaiting a model");
            return 0;
        }
    };

    let mut written = 0;
    for session in waiting {
        // The scope comes from the checkout the session was recorded in, the
        // way the reaper resolves one. A working copy that has since been
        // moved or unmounted leaves the session where it is: the page it would
        // be written to is the thing that cannot be located.
        let Ok(scope) = resolve_scope(&session.checkout_path) else {
            tracing::debug!(
                session = %session.id,
                path = %session.checkout_path.display(),
                "left a session alone: its checkout no longer resolves to a project"
            );
            continue;
        };

        match enrich(
            &state.store,
            &state.wiki,
            &scope,
            session.id,
            state.embedder.clone(),
            now,
            &settings,
        )
        .await
        {
            Ok(Enriched::Wrote(page)) => {
                tracing::info!(%page, session = %session.id, "asked again, and this time a model answered");
                written += 1;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, session = %session.id, "could not ask again about a session")
            }
        }
    }
    written
}

/// Retry forever, on the same tick as the other background passes.
///
/// Unlike the reaper this waits before its first pass. The sessions it revisits
/// have pages already and are in no hurry, and a server that has just started
/// is the worst moment to spend a rate limit — the enrichment that runs inline
/// after each session ends is the fast path, and this is only the net beneath
/// it.
pub async fn run_enricher(state: AppState) {
    loop {
        tokio::time::sleep(TICK).await;
        sweep_awaiting(&state, Timestamp::now()).await;
    }
}
