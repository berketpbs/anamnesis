//! Writing up the session somebody just walked away from.
//!
//! Consolidation hangs off `SessionEnd`, and a terminal that is closed sends
//! nothing. The session stays open, and until [`crate::reap`] gives up on it
//! twelve hours later there is no page and no handoff — so the agent the person
//! opens a minute later is handed silence, which is also what it is handed when
//! there was no previous session at all. Measured on this project's own memory:
//! of 131 closed sessions, 89 ended within a minute of their last event and 42
//! ended more than six hours after it, with nothing in between; twelve real
//! handovers had the note they needed written between nine and twenty-one hours
//! after they had already started.
//!
//! The reaper is right that silence alone cannot tell a session that died from
//! one whose operator went to lunch, which is why it waits. Here something else
//! is known: **somebody has just started working in the same slot**. That is the
//! evidence the reaper does not have, and it is what this pass acts on.
//!
//! It does not end the session. In this project's history live sessions went
//! quiet for more than two minutes 417 times — a long build is one silence, and
//! so is a terminal left open over lunch — so silence proves nothing about an
//! end even when somebody else sits down. The page is refreshed and a note is
//! left beside it; the session ends when its harness says so, and that rewrites
//! the page and supersedes the note through the ordinary path.
//!
//! Everything here is bounded, because a person is waiting on the other end of
//! it: at most [`AT_MOST`] sessions per claim, [`BUDGET`] between them, and the
//! wiki taken with a timeout. A claim is never failed by this pass — the worst
//! outcome it may produce is the silence that happens without it.

use std::time::{Duration, Instant};

use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::SessionId;
use anamnesis_core::scope::ResolvedScope;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::WebError;
use crate::pipeline;

/// How many quiet sessions one claim will write up.
///
/// More than one because two terminals left open is ordinary; bounded because
/// this runs while a new session waits for its answer. Whatever is left over is
/// written up by the next claim, or by the reaper.
const AT_MOST: usize = 4;

/// How much of the claim's second this pass may spend before stopping.
///
/// Checked *between* sessions, never before the first: the common case is one
/// quiet session, and a budget that could skip it would make the whole pass a
/// coin toss. The hook's own budget is one second (`anamnesis-cli`'s capture
/// path), and what it does when that runs out is print a notice and carry on.
const BUDGET: Duration = Duration::from_millis(750);

/// How long to wait for the wiki when something else is writing to it.
///
/// A commit elsewhere is a good reason to skip this pass and answer the claim
/// with whatever is already pending, and a bad reason to make a person wait.
const FOR_THE_WIKI: Duration = Duration::from_millis(200);

/// The sessions this claim should write up: open, in the same slot, quiet for
/// long enough, not the claimant itself, and not already written up since
/// their last event.
///
/// That last condition is what keeps a note single-use. A session handed over
/// to the terminal opened after it is still open and still quiet when a third
/// one starts; writing it up again would hand the third person the note the
/// second already took, and commit the same page twice. Once it moves again
/// it has something new to say, and is written up again.
///
/// Oldest silence first, so the session that stopped most recently records its
/// note last and holds the slot. `record_handoff` supersedes an unread note, so
/// the order decides which one the claim takes.
fn quiet_peers(
    store: &Store,
    scope: &ResolvedScope,
    claimant: SessionId,
    slot: &Slot,
    now: Timestamp,
) -> Result<Vec<SessionId>, WebError> {
    let after = i64::from(scope.sessions.handover_after_seconds);
    if after == 0 {
        return Ok(Vec::new());
    }

    let mut quiet: Vec<(Timestamp, SessionId)> = Vec::new();
    for open in store.open_sessions()? {
        if open.id == claimant {
            continue;
        }
        if now.as_second() - open.last_seen.as_second() < after {
            continue;
        }
        if store
            .last_handoff_from(open.id)?
            .is_some_and(|written| written >= open.last_seen)
        {
            continue;
        }
        let Some(session) = store.load_session(open.id)? else {
            continue;
        };
        if session.project_id != scope.project_id || pipeline::slot_for(scope, &session) != *slot {
            continue;
        }
        quiet.push((open.last_seen, open.id));
    }

    quiet.sort_by_key(|(last_seen, id)| (*last_seen, *id));
    quiet.truncate(AT_MOST);
    Ok(quiet.into_iter().map(|(_, id)| id).collect())
}

/// Write up the quiet sessions in this slot, and say how many were written.
///
/// Called with the index and the wiki in hand, on the thread that is about to
/// answer a claim. Failures are logged and swallowed: a session that could not
/// be written up is a session the next claim or the reaper will write up, and
/// nothing here is worth failing a claim over.
pub(crate) fn hand_over_peers(
    store: &Store,
    wiki: &parking_lot::Mutex<anamnesis_wiki::Wiki>,
    embed_model: Option<&str>,
    scope: &ResolvedScope,
    claimant: SessionId,
    slot: &Slot,
    now: Timestamp,
) -> usize {
    let peers = match quiet_peers(store, scope, claimant, slot, now) {
        Ok(peers) => peers,
        Err(error) => {
            tracing::warn!(%error, "could not look for sessions to hand over");
            return 0;
        }
    };

    let started = Instant::now();
    let mut written = 0;
    for (index, peer) in peers.iter().enumerate() {
        if index > 0 && started.elapsed() > BUDGET {
            tracing::debug!(
                left = peers.len() - index,
                "stopped handing sessions over: a claim was waiting"
            );
            break;
        }
        let Some(held) = wiki.try_lock_for(FOR_THE_WIKI) else {
            tracing::debug!("left a quiet session for later: the wiki is busy");
            break;
        };
        match pipeline::hand_over(store, &held, scope, *peer, embed_model, now) {
            Ok(Some(page)) => {
                tracing::info!(
                    session = %peer,
                    project = %scope.scope,
                    %page,
                    "wrote up a quiet session for the one that just started"
                );
                written += 1;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                session = %peer,
                "could not write up a quiet session; it keeps its place in the queue"
            ),
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    use anamnesis_core::observation::{BoundedBody, EventKind};
    use anamnesis_core::scope::resolve_scope;
    use anamnesis_core::session::AgentKind;
    use anamnesis_store::{Store, new_observation, new_session};
    use anamnesis_wiki::Wiki;

    use crate::AppState;

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        state: AppState,
        scope: ResolvedScope,
    }

    fn harness_with(extra: &str) -> Harness {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            format!("[scope]\nworkspace = \"default\"\nproject = \"widget\"\n{extra}"),
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");

        let data = tempfile::tempdir().expect("data");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        store.upsert_project(&scope, now()).expect("project");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");

        Harness {
            state: AppState::new(store, wiki),
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn harness() -> Harness {
        harness_with("")
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    fn seconds_ago(seconds: i64) -> Timestamp {
        now() - jiff::Span::new().seconds(seconds)
    }

    /// An open session whose newest observation is `seconds` old, carrying one
    /// request so that it is worth a page at all.
    fn session_quiet_for(harness: &Harness, name: &str, seconds: i64) -> SessionId {
        let id = SessionId::derive(harness.scope.project_id, name);
        let session = new_session(
            id,
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            harness.scope.root.clone(),
            seconds_ago(seconds + 60),
            None,
        );
        harness
            .state
            .store
            .ensure_session(&session)
            .expect("session");
        harness
            .state
            .store
            .insert_observation(&new_observation(
                id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating(format!("make {name} pass"), 1024),
                seconds_ago(seconds),
            ))
            .expect("observation");
        id
    }

    fn slot() -> Slot {
        Slot::for_workstream(None).for_operator(None)
    }

    /// The claimant, as `pipeline::claimant` would have recorded it.
    fn starting(harness: &Harness) -> SessionId {
        starting_as(harness, "the-new-one")
    }

    fn starting_as(harness: &Harness, name: &str) -> SessionId {
        let id = SessionId::derive(harness.scope.project_id, name);
        let session = new_session(
            id,
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::Codex,
            harness.scope.root.clone(),
            now(),
            None,
        );
        harness
            .state
            .store
            .ensure_session(&session)
            .expect("session");
        id
    }

    fn sweep(harness: &Harness, claimant: SessionId, embed_model: Option<&str>) -> usize {
        hand_over_peers(
            &harness.state.store,
            &harness.state.wiki,
            embed_model,
            &harness.scope,
            claimant,
            &slot(),
            now(),
        )
    }

    fn claimed(harness: &Harness, claimant: SessionId) -> Option<String> {
        harness
            .state
            .store
            .claim_handoff(harness.scope.project_id, claimant, &slot(), now())
            .expect("claim")
    }

    fn state_of(harness: &Harness, id: SessionId) -> String {
        harness
            .state
            .store
            .recent_sessions(harness.scope.project_id, 10)
            .expect("list")
            .into_iter()
            .find(|row| row.id == id)
            .expect("session")
            .state
    }

    /// The case the module exists for: a terminal closed without ending its
    /// session, and the agent opened beside it a minute later.
    #[test]
    fn a_new_session_is_handed_what_the_quiet_one_left() {
        let harness = harness();
        let quiet = session_quiet_for(&harness, "the-closed-terminal", 180);
        let new = starting(&harness);

        assert_eq!(sweep(&harness, new, None), 1);

        let note = claimed(&harness, new).expect("a note to take");
        assert!(note.contains("make the-closed-terminal pass"), "{note}");
        assert_eq!(
            state_of(&harness, quiet),
            "open",
            "handing over is not ending"
        );
    }

    /// Two terminals open at once is a working pattern, not a fault, and a
    /// session is silent for as long as the command it is waiting on.
    #[test]
    fn a_session_that_is_merely_between_tools_is_left_alone() {
        let harness = harness();
        let quiet = session_quiet_for(&harness, "still-working", 30);
        let new = starting(&harness);

        assert_eq!(sweep(&harness, new, None), 0);

        assert!(claimed(&harness, new).is_none());
        assert_eq!(state_of(&harness, quiet), "open");
    }

    #[test]
    fn a_session_does_not_write_itself_up() {
        let harness = harness();
        let id = session_quiet_for(&harness, "the-only-one", 600);

        assert_eq!(sweep(&harness, id, None), 0);
        assert!(claimed(&harness, id).is_none());
    }

    #[test]
    fn a_project_can_turn_it_off() {
        let harness = harness_with("[sessions]\nhandover_after_seconds = 0\n");
        session_quiet_for(&harness, "the-closed-terminal", 60 * 60 * 24);
        let new = starting(&harness);

        assert_eq!(sweep(&harness, new, None), 0);
        assert!(claimed(&harness, new).is_none());
    }

    /// A note about a session that has not finished must not read as one about
    /// a session that has.
    #[test]
    fn the_note_says_the_session_is_still_open() {
        let harness = harness();
        session_quiet_for(&harness, "the-closed-terminal", 180);
        let new = starting(&harness);
        sweep(&harness, new, None);

        let note = claimed(&harness, new).expect("a note to take");

        assert!(note.contains("still open"), "{note}");
        assert!(!note.contains("Previous session"), "{note}");
    }

    /// The page is written without waiting for the embedder, and a page nobody
    /// tried to embed is invisible to the pass that fills vectors in — so the
    /// attempt is recorded in its place.
    #[test]
    fn the_page_keeps_its_place_in_the_vector_stream() {
        let harness = harness();
        let quiet = session_quiet_for(&harness, "the-closed-terminal", 180);
        let new = starting(&harness);

        sweep(&harness, new, Some("nomic-embed-text"));

        let waiting = harness
            .state
            .store
            .pages_missing_vectors("nomic-embed-text", 10)
            .expect("waiting");
        let session = harness
            .state
            .store
            .load_session(quiet)
            .expect("load")
            .expect("session");
        let page = crate::pipeline::session_page_path(&session.started_at, quiet).expect("path");
        assert!(
            waiting.iter().any(|(_, path)| *path == page),
            "{waiting:?} should be waiting for a vector"
        );
        assert_eq!(
            harness
                .state
                .store
                .pages_from_session(quiet)
                .expect("pages"),
            vec![page],
            "and the page itself should be in the index"
        );
    }

    /// Two quiet sessions, and only one slot. The one that stopped most
    /// recently is the one a person is most likely to be carrying on from.
    #[test]
    fn the_session_that_stopped_last_holds_the_slot() {
        let harness = harness();
        session_quiet_for(&harness, "older", 900);
        session_quiet_for(&harness, "newer", 180);
        let new = starting(&harness);

        assert_eq!(sweep(&harness, new, None), 2);

        let note = claimed(&harness, new).expect("a note to take");
        assert!(note.contains("make newer pass"), "{note}");
    }

    /// A third terminal opened beside a session that was already handed over
    /// is not handed its note a second time: the person in the second terminal
    /// took it, and the session has not moved since.
    #[test]
    fn a_quiet_session_is_handed_over_once() {
        let harness = harness();
        session_quiet_for(&harness, "the-closed-terminal", 180);
        let second = starting(&harness);
        assert_eq!(sweep(&harness, second, None), 1);
        assert!(claimed(&harness, second).is_some());

        let third = starting_as(&harness, "the-third-one");
        assert_eq!(sweep(&harness, third, None), 0);
        assert!(claimed(&harness, third).is_none());
    }

    /// Once it moves again it has something new to say.
    #[test]
    fn a_session_that_moved_since_its_handover_is_written_up_again() {
        let harness = harness();
        let quiet = session_quiet_for(&harness, "the-closed-terminal", 900);
        let second = starting(&harness);
        hand_over_peers(
            &harness.state.store,
            &harness.state.wiki,
            None,
            &harness.scope,
            second,
            &slot(),
            seconds_ago(600),
        );
        assert!(claimed(&harness, second).is_some());

        harness
            .state
            .store
            .insert_observation(&new_observation(
                quiet,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating("now make the exporter pass", 1024),
                seconds_ago(300),
            ))
            .expect("observation");

        let third = starting_as(&harness, "the-third-one");
        assert_eq!(sweep(&harness, third, None), 1);
        let note = claimed(&harness, third).expect("a note to take");
        assert!(note.contains("exporter"), "{note}");
    }
}
