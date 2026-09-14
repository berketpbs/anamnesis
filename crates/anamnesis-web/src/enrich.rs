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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anamnesis_consolidate::{Surroundings, consolidate_attributed};
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
            // Both come from the wiki, so both are read under one hold of it.
            // The session's own page is left out of the list: it exists by now
            // — `finalize` wrote it — and a page that links to itself has said
            // nothing.
            let (preferences, pages) = {
                let wiki = wiki.lock();
                let own = crate::pipeline::session_page_path(&session.started_at, session.id)?;
                let pages = wiki
                    .pages(&scope.scope)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|path| path != &own)
                    .map(|path| path.as_str().to_owned())
                    .collect::<Vec<_>>();
                (read_preferences(&wiki, &scope), pages)
            };
            Ok(Some((session, observations, preferences, pages)))
        })
        .await?
    };
    let Some((session, observations, preferences, pages)) = loaded else {
        return Ok(Enriched::Nothing);
    };

    let compiled = consolidate_attributed(
        llm.provider.as_ref(),
        &session,
        &observations,
        Surroundings {
            preferences: preferences.as_deref(),
            pages: &pages,
        },
        llm.max_input_tokens,
        llm.max_output_tokens,
    )
    .await;

    let Some(compiled) = compiled else {
        return Ok(Enriched::Nothing);
    };
    // The model that wrote the page, which with a chain configured is not
    // always the one the provider is named after.
    let model = compiled.model(llm.provider.as_ref()).to_owned();
    let (digest, source) = (compiled.digest, compiled.source);

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

        Ok(Enriched::Wrote(page.page))
    })
    .await
}

/// How many sessions awaiting a model are looked at to find [`BATCH`] that are
/// due. Every counted session a memory holds, in practice: the query is one
/// indexed scan, and a window no wider than the batch is what let three
/// sessions nothing could enrich stand in front of every other one.
const CANDIDATES: usize = 1000;

/// The longest one session waits between two attempts.
const SESSION_WAIT_CAP: Duration = Duration::from_secs(6 * 60 * 60);

/// The longest the enricher sleeps after passes in which nothing answered.
const QUIET_WAIT_CAP: Duration = Duration::from_secs(60 * 60);

/// When to ask again, and about whom.
///
/// The pass used to run every [`TICK`] over the three oldest counted sessions,
/// whatever had happened to them. On 2026-09-14 the model key on the machine
/// this project runs on stopped being accepted, and from 00:11 the server
/// asked Google about the same three sessions every minute — 820 refused
/// requests by 15:05, each one also trying the fallback — and would have gone
/// on doing it. With a key that works and a daily quota spent, the same loop
/// spends every minute of the rest of the day on requests that cannot succeed.
/// And a session that can never be enriched — its checkout moved, or nothing
/// in it — held one of the three places for good, so a memory with three of
/// them never had another session looked at.
///
/// Two answers. A session that did not get a page waits before it is asked
/// about again, twice as long each time, up to six hours, so the queue moves
/// past it. And a pass in which the model was asked and wrote nothing at all
/// doubles the pause before the next pass, up to an hour, so an outage costs a
/// few requests an hour instead of three a minute. A page written resets both.
/// Held in memory: a restart asks straight away, which is what somebody who
/// restarted a server after fixing its key expects.
#[derive(Debug, Default)]
pub struct Pacing {
    waiting: HashMap<SessionId, (u32, Timestamp)>,
    quiet_passes: u32,
}

impl Pacing {
    /// Whether a session may be asked about at `now`.
    pub fn is_due(&self, session: SessionId, now: Timestamp) -> bool {
        self.waiting
            .get(&session)
            .is_none_or(|(_, not_before)| now >= *not_before)
    }

    /// A session came back without a page.
    pub fn missed(&mut self, session: SessionId, now: Timestamp) {
        let attempts = self.waiting.get(&session).map_or(0, |(n, _)| *n) + 1;
        let wait = doubled(TICK, attempts, SESSION_WAIT_CAP);
        let not_before = now
            .checked_add(jiff::SignedDuration::try_from(wait).unwrap_or(jiff::SignedDuration::MAX))
            .unwrap_or(Timestamp::MAX);
        self.waiting.insert(session, (attempts, not_before));
    }

    /// A session got its page.
    pub fn wrote(&mut self, session: SessionId) {
        self.waiting.remove(&session);
    }

    /// One pass is over: `asked` sessions went to the model and `written` came
    /// back as pages. A pass that asked nobody says nothing about the model.
    pub fn finished(&mut self, asked: usize, written: usize) {
        if written > 0 {
            self.quiet_passes = 0;
        } else if asked > 0 {
            self.quiet_passes = self.quiet_passes.saturating_add(1);
        }
    }

    /// How long to sleep before the next pass.
    pub fn next_pass_in(&self) -> Duration {
        doubled(TICK, self.quiet_passes, QUIET_WAIT_CAP)
    }
}

/// `base` doubled `times` times, no longer than `cap`.
fn doubled(base: Duration, times: u32, cap: Duration) -> Duration {
    base.checked_mul(1u32.checked_shl(times).unwrap_or(u32::MAX))
        .map_or(cap, |wait| wait.min(cap))
}

/// Ask again about the sessions a model never answered for.
///
/// One pass, over a handful of sessions at a time. Returns how many pages it
/// wrote, which is what the tests assert on and what the log line reports.
pub async fn sweep_awaiting(state: &AppState, now: Timestamp) -> usize {
    sweep_paced(state, now, &mut Pacing::default()).await
}

/// [`sweep_awaiting`], skipping sessions `pacing` says are not due and telling
/// it how each one went.
pub async fn sweep_paced(state: &AppState, now: Timestamp, pacing: &mut Pacing) -> usize {
    let Some(settings) = state.llm.clone() else {
        return 0;
    };

    let waiting = match state.store.sessions_awaiting_enrichment(CANDIDATES) {
        Ok(waiting) => waiting,
        Err(error) => {
            tracing::error!(%error, "could not list the sessions awaiting a model");
            return 0;
        }
    };

    let due: Vec<_> = waiting
        .into_iter()
        .filter(|session| pacing.is_due(session.id, now))
        .take(BATCH)
        .collect();

    let mut written = 0;
    let mut asked = 0;
    for session in due {
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
            pacing.missed(session.id, now);
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
                pacing.wrote(session.id);
                asked += 1;
                written += 1;
            }
            Ok(Enriched::Counted) => {
                pacing.missed(session.id, now);
                asked += 1;
            }
            Ok(Enriched::Nothing) => pacing.missed(session.id, now),
            Err(error) => {
                tracing::error!(%error, session = %session.id, "could not ask again about a session");
                pacing.missed(session.id, now);
            }
        }
    }
    pacing.finished(asked, written);
    written
}

/// Retry forever, on the same tick as the other background passes while a
/// model answers, and further apart while it does not (see [`Pacing`]).
///
/// Unlike the reaper this waits before its first pass. The sessions it revisits
/// have pages already and are in no hurry, and a server that has just started
/// is the worst moment to spend a rate limit — the enrichment that runs inline
/// after each session ends is the fast path, and this is only the net beneath
/// it.
pub async fn run_enricher(state: AppState) {
    let mut pacing = Pacing::default();
    loop {
        let wait = pacing.next_pass_in();
        if wait > TICK {
            tracing::warn!(
                minutes = wait.as_secs() / 60,
                "no model has answered the sessions asked about again; waiting longer before the next pass"
            );
        }
        tokio::time::sleep(wait).await;
        sweep_paced(&state, Timestamp::now(), &mut pacing).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(_: u8) -> SessionId {
        SessionId::new()
    }

    fn after(start: Timestamp, wait: Duration) -> Timestamp {
        start
            .checked_add(jiff::SignedDuration::try_from(wait).unwrap())
            .unwrap()
    }

    /// Twice as long each time, and never longer than six hours: a session
    /// refused a hundred times is still asked about four times a day, since
    /// the reason may have been fixed.
    #[test]
    fn a_session_waits_twice_as_long_each_time_up_to_six_hours() {
        let mut pacing = Pacing::default();
        let start = Timestamp::UNIX_EPOCH;
        let id = session(1);
        assert!(pacing.is_due(id, start), "never asked about is due");

        pacing.missed(id, start);
        assert!(!pacing.is_due(id, after(start, TICK)));
        assert!(pacing.is_due(id, after(start, TICK * 2)));

        pacing.missed(id, start);
        assert!(!pacing.is_due(id, after(start, TICK * 3)));
        assert!(pacing.is_due(id, after(start, TICK * 4)));

        for _ in 0..100 {
            pacing.missed(id, start);
        }
        assert!(!pacing.is_due(id, after(start, SESSION_WAIT_CAP - TICK)));
        assert!(pacing.is_due(id, after(start, SESSION_WAIT_CAP)));

        pacing.wrote(id);
        assert!(
            pacing.is_due(id, start),
            "a page written forgets the misses"
        );
        assert!(
            pacing.is_due(session(2), start),
            "and nobody else was held back"
        );
    }

    #[test]
    fn passes_in_which_nothing_answered_move_apart_up_to_an_hour() {
        let mut pacing = Pacing::default();
        assert_eq!(pacing.next_pass_in(), TICK);

        pacing.finished(3, 0);
        assert_eq!(pacing.next_pass_in(), TICK * 2);
        pacing.finished(0, 0);
        assert_eq!(
            pacing.next_pass_in(),
            TICK * 2,
            "a pass that asked nobody says nothing about the model"
        );

        for _ in 0..40 {
            pacing.finished(1, 0);
        }
        assert_eq!(pacing.next_pass_in(), QUIET_WAIT_CAP);

        pacing.finished(3, 1);
        assert_eq!(
            pacing.next_pass_in(),
            TICK,
            "one page written and it is back to every minute"
        );
    }
}
