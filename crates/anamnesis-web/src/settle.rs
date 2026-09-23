//! Writing down what a session decided, before it ends.
//!
//! Notes — the decisions, gotchas and procedures a session leaves beside its
//! page — are written by the model that summarises a session, and that happens
//! when the session ends: on `SessionEnd`, or twelve hours later when the
//! reaper gives up on a terminal somebody closed. Until then a decision taken
//! in conversation exists only in the transcript. The case that matters is the
//! ordinary one: a person settles something with Claude, closes it, and opens
//! Codex a minute later. Codex is handed a note counted from the transcript,
//! and the decision is in no page it could be told about.
//!
//! So this pass looks for the moment a session has finished a turn and nobody
//! has said anything since — the agent answered, and `notes_after_seconds`
//! passed — and asks a model once for the notes the session has left so far.
//! Only the notes: the page is refreshed and the handoff left by the paths that
//! already do that, and neither is anything a person waiting on the other side
//! of a handover should wait on a model for.
//!
//! Three things keep it from costing more than it is worth.
//!
//! * **Once per silence.** A session is asked again only after it has moved.
//!   Held in memory: a restart asks again at most once per open session.
//! * **Only a finished turn.** A session whose last event is a tool call or an
//!   unanswered prompt is at work; a long build is a silence too.
//! * **Told what it already wrote.** The model sees the notes this session
//!   left before, so a decision that still holds keeps its title and its page
//!   is updated rather than repeated when the session is asked again at its
//!   end.

use std::collections::HashMap;

use anamnesis_consolidate::{Surroundings, consolidate_attributed};
use anamnesis_core::embedding::Embed;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::EventKind;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::SummarySource;
use jiff::Timestamp;

use crate::enrich::notes_left_by;
use crate::improve::TICK;
use crate::pipeline::{read_preferences, summary_source, write_notes};
use crate::{AppState, WebError};

/// When each session was last asked about: the `last_seen` it had then.
///
/// A session asked about at one `last_seen` is not asked again until that
/// moves, which is what "once per silence" means.
pub type Asked = HashMap<SessionId, Timestamp>;

/// What one pass did, for the tests and the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettleReport {
    /// Sessions a model was asked about, with the notes that came back.
    pub asked: Vec<(SessionId, Vec<String>)>,
}

/// Ask about every open session that has finished a turn and gone quiet.
pub async fn settle(state: &AppState, now: Timestamp, asked: &mut Asked) -> SettleReport {
    let mut report = SettleReport::default();
    let Some(llm) = state.llm.clone() else {
        return report;
    };

    let store = state.store.clone();
    let open = match crate::off_runtime(move || Ok::<_, WebError>(store.open_sessions()?)).await {
        Ok(open) => open,
        Err(error) => {
            tracing::error!(%error, "could not list the open sessions to write their notes");
            return report;
        }
    };
    // A session that is no longer open is no longer this pass's to remember.
    asked.retain(|id, _| open.iter().any(|session| session.id == *id));

    for session in open {
        if asked.get(&session.id) == Some(&session.last_seen) {
            continue;
        }
        let checkout = session.checkout_path.clone();
        let store = state.store.clone();
        let id = session.id;
        let due = crate::off_runtime(move || -> Result<_, WebError> {
            let Ok(scope) = resolve_scope(&checkout) else {
                return Ok(None);
            };
            let after = i64::from(scope.sessions.notes_after_seconds);
            if after == 0 || now.as_second() - session.last_seen.as_second() < after {
                return Ok(None);
            }
            if store.last_event_kind(id)? != Some(EventKind::AssistantMessage) {
                return Ok(None);
            }
            Ok(Some(scope))
        })
        .await;
        let scope = match due {
            Ok(Some(scope)) => scope,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, session = %session.id, "could not tell whether a quiet session is due its notes");
                continue;
            }
        };

        // One question about a session at a time, shared with the enricher:
        // a session that ends while this is being asked is asked about by
        // whichever gets there first, not by both.
        let Some(_claim) = llm.asking.claim(session.id) else {
            continue;
        };
        asked.insert(session.id, session.last_seen);

        let loaded = {
            let store = state.store.clone();
            let wiki = state.wiki.clone();
            let scope = scope.clone();
            crate::off_runtime(move || -> Result<_, WebError> {
                let Some(loaded) = store.load_session(id)? else {
                    return Ok(None);
                };
                if !loaded.is_open() {
                    return Ok(None);
                }
                let observations = store.observations(id)?;
                let held = wiki.lock();
                let pages = held
                    .pages(&scope.scope)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|path| !path.is_session_record())
                    .map(|path| path.as_str().to_owned())
                    .collect::<Vec<_>>();
                let own_notes = notes_left_by(&store, &held, &scope, &loaded)?;
                Ok(Some((
                    loaded,
                    observations,
                    read_preferences(&held, &scope),
                    pages,
                    own_notes,
                )))
            })
            .await
        };
        let (loaded, observations, preferences, pages, own_notes) = match loaded {
            Ok(Some(loaded)) => loaded,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, session = %session.id, "could not read a quiet session");
                continue;
            }
        };

        let Some(compiled) = consolidate_attributed(
            llm.provider.as_ref(),
            &loaded,
            &observations,
            Surroundings {
                preferences: preferences.as_deref(),
                pages: &pages,
                own_notes: &own_notes,
            },
            llm.max_input_tokens,
            llm.max_output_tokens,
        )
        .await
        else {
            continue;
        };
        // Counted notes are none: counting cannot tell a decision from a
        // sentence, which is why notes are asked of a model at all.
        if summary_source(compiled.source) != SummarySource::Model
            || compiled.digest.notes.is_empty()
        {
            report.asked.push((session.id, Vec::new()));
            continue;
        }

        let store = state.store.clone();
        let wiki = state.wiki.clone();
        let embedder = state.embedder.clone();
        let digest = compiled.digest;
        let written = crate::off_runtime(move || -> Result<_, WebError> {
            let held = wiki.lock();
            // Still open under the wiki's lock: a session that ended while the
            // model was being asked has been, or is being, written up whole,
            // and its own notes are that path's to write.
            match store.load_session(id)? {
                Some(current) if current.is_open() => {}
                _ => return Ok(Vec::new()),
            }
            Ok(write_notes(
                &store,
                &held,
                &scope,
                &loaded,
                &digest,
                embedder
                    .as_ref()
                    .map(|embedder| embedder.as_ref() as &dyn Embed),
                now,
            ))
        })
        .await
        .unwrap_or_default();

        if !written.is_empty() {
            tracing::info!(
                session = %session.id,
                notes = ?written,
                "wrote down what a quiet session decided"
            );
        }
        report.asked.push((session.id, written));
    }
    report
}

/// Look for quiet sessions forever.
///
/// On the reaper's tick. It returns at once on a server with no model, and
/// every pass over one that has a model but no quiet session is one query.
pub async fn run_settler(state: AppState) {
    let mut asked = Asked::new();
    loop {
        let passing = state.clone();
        let mut held = std::mem::take(&mut asked);
        // A pass that panicked forgets what it had asked, which costs at most
        // one question per open session; the loop carries on either way.
        asked = crate::one_pass("settler", async move {
            settle(&passing, Timestamp::now(), &mut held).await;
            held
        })
        .await
        .unwrap_or_default();
        tokio::time::sleep(TICK).await;
    }
}
