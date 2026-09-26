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
use anamnesis_core::observation::EventKind;
use anamnesis_core::scope::ResolvedScope;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::WebError;
use crate::pipeline;

/// How many sessions one claim will try before giving up on writing any.
///
/// More than one because the newest session beside a new one may have nothing
/// worth a page — a terminal opened and closed again, say — and the one before
/// it does. Bounded because this runs while a new session waits for its
/// answer.
const AT_MOST: usize = 4;

/// How much of the claim's second this pass may spend before stopping.
///
/// Checked *between* sessions, never before the first: the common case is one
/// session, and a budget that could skip it would make the whole pass a coin
/// toss. The hook's own budget is one second (`anamnesis-cli`'s capture path),
/// and what it does when that runs out is print a notice and carry on.
const BUDGET: Duration = Duration::from_millis(750);

/// How long to wait for the wiki when something else is writing to it.
///
/// A commit elsewhere is a good reason to skip this pass and answer the claim
/// with whatever is already pending, and a bad reason to make a person wait.
const FOR_THE_WIKI: Duration = Duration::from_millis(200);

/// What a sweep did, for the claim that comes after it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Swept {
    /// Whether a session was written up, and so holds the slot now.
    pub written: bool,
    /// Whether a session newer than the slot's last note was left unwritten
    /// for a reason that was not its own: the wiki was busy, the budget ran
    /// out, the write failed.
    ///
    /// Then the slot's last note is not the latest account of it, and must not
    /// be handed on as though it were — that would put older work in front of
    /// newer, the mistake this module exists to prevent.
    pub unsettled: bool,
}

impl Swept {
    const UNSETTLED: Self = Self {
        written: false,
        unsettled: true,
    };
}

/// The sessions this claim may write up, newest first: open, in the same
/// slot, not the claimant itself, active since the slot's last note was
/// written, and not in the middle of a turn.
///
/// **Active since the last note, whatever became of it.** A session that went
/// quiet before the slot's newest note was written has nothing to say that the
/// note's author did not come after. Once that was checked against the note
/// *waiting* in the slot, and it was not enough: observed live, a Claude
/// session ended and left its note, the Codex opened next took it, and the
/// Claude opened a minute after that was handed a Codex terminal that had been
/// sitting quiet for thirty-five minutes — older than everything the person had
/// just been looking at. The note had been taken, so nothing was waiting, so
/// nothing stood in the way. A note that was taken is still the newest account
/// of the slot.
///
/// This is also what keeps a write-up from being handed out twice: its note is
/// written after the session's last event, so until the session moves again it
/// is not a candidate.
///
/// **Not in the middle of a turn.** A session whose last event is the agent's
/// answer has done what it was asked and is waiting for its person, and is
/// handed over at once: somebody who asks Codex something, reads the answer
/// and opens Claude thirty seconds later is carrying on from that answer. A
/// session last seen running a tool, or with a prompt still unanswered, is at
/// work — two terminals open at once is a working pattern, and a long build is
/// a silence. That one waits out `handover_after_seconds` first, which is long
/// enough to tell a terminal closed mid-turn from one still busy.
fn candidates(
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

    let noted = store
        .latest_handoff(scope.project_id, slot)?
        .map(|note| note.as_of);
    let mut found: Vec<(Timestamp, SessionId)> = Vec::new();
    for open in store.open_sessions()? {
        if open.id == claimant {
            continue;
        }
        if noted.is_some_and(|written| written >= open.last_seen) {
            continue;
        }
        let Some(session) = store.load_session(open.id)? else {
            continue;
        };
        if session.project_id != scope.project_id || pipeline::slot_for(scope, &session) != *slot {
            continue;
        }
        let answered = store.last_event_kind(open.id)? == Some(EventKind::AssistantMessage);
        if !answered && now.as_second() - open.last_seen.as_second() < after {
            continue;
        }
        found.push((open.last_seen, open.id));
    }

    found.sort_by_key(|(last_seen, id)| std::cmp::Reverse((*last_seen, *id)));
    Ok(found.into_iter().map(|(_, id)| id).collect())
}

/// Write up the newest session beside this one that has anything to say.
///
/// One, not all of them: every write-up records a note and the newer
/// supersedes the older, so writing more would only commit pages nobody is
/// waiting for while somebody waits. The rest keep their place for their own
/// `SessionEnd`, or the reaper.
///
/// Called with the index and the wiki in hand, on the thread that is about to
/// answer a claim. Failures are logged and swallowed — nothing here is worth
/// failing a claim over — and reported in [`Swept::unsettled`] instead.
pub(crate) fn hand_over_peers(
    store: &Store,
    wiki: &parking_lot::Mutex<anamnesis_wiki::Wiki>,
    embed_model: Option<&str>,
    scope: &ResolvedScope,
    claimant: SessionId,
    slot: &Slot,
    now: Timestamp,
) -> Swept {
    let peers = match candidates(store, scope, claimant, slot, now) {
        Ok(peers) => peers,
        Err(error) => {
            tracing::warn!(%error, "could not look for sessions to hand over");
            return Swept::UNSETTLED;
        }
    };

    let started = Instant::now();
    for (index, peer) in peers.iter().enumerate() {
        if index >= AT_MOST || (index > 0 && started.elapsed() > BUDGET) {
            tracing::debug!(
                left = peers.len() - index,
                "stopped handing sessions over: a claim was waiting"
            );
            return Swept::UNSETTLED;
        }
        let Some(held) = wiki.try_lock_for(FOR_THE_WIKI) else {
            tracing::debug!("left a session for later: the wiki is busy");
            return Swept::UNSETTLED;
        };
        match pipeline::hand_over(store, &held, scope, *peer, embed_model, now) {
            Ok(Some(page)) => {
                tracing::info!(
                    session = %peer,
                    project = %scope.scope,
                    %page,
                    "wrote up an open session for the one that just started"
                );
                return Swept {
                    written: true,
                    unsettled: false,
                };
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    session = %peer,
                    "could not write up an open session; it keeps its place in the queue"
                );
                return Swept::UNSETTLED;
            }
        }
    }
    Swept::default()
}

/// The slot's newest note again, for a session that found nothing waiting.
///
/// A note is claimed once, and that is right for two sessions starting at the
/// same instant. It was wrong for the person it is for: Claude ends, Codex
/// opens and takes the note, the person closes Codex a minute later having
/// read it and done nothing, opens Claude — and is told nothing, because the
/// note was taken. The note is still the newest account of the slot, since
/// nobody has done anything after it. So it is handed on.
///
/// Not when the sweep was unsettled (something newer may exist), not when the
/// newest note was dropped on purpose, not to the session that wrote it or
/// already took it, and not when it describes nothing after `known` — the
/// claimant's own last work, see [`handed`].
pub(crate) fn hand_on(
    store: &Store,
    scope: &ResolvedScope,
    claimant: SessionId,
    slot: &Slot,
    swept: Swept,
    known: Option<Timestamp>,
) -> Result<Option<String>, WebError> {
    if swept.unsettled || scope.sessions.handover_after_seconds == 0 {
        return Ok(None);
    }
    let Some(note) = store.latest_handoff(scope.project_id, slot)? else {
        return Ok(None);
    };
    if note.dropped
        || note.from_session == claimant
        || note.taken_by == Some(claimant)
        || old_news(note.as_of, known)
    {
        return Ok(None);
    }
    Ok(Some(note.body))
}

/// What a starting session is handed: the note waiting in its slot, or else
/// the slot's newest note again ([`hand_on`]) — and neither when it describes
/// no work after the session's own.
///
/// A session that starts for the first time has done nothing, and anything is
/// news to it. A session that is *resumed* has its whole conversation back,
/// and a note about work older than its own last step tells it less than it
/// already knows, with the authority of a handover: on 2026-09-26 a resumed
/// Claude session, an hour of work past a Codex terminal's last answer, was
/// handed that terminal's note — the commit before the one it had released,
/// and the Codex session's instruction to change nothing. So the claimant's
/// last work before this start is what a note has to be newer than. That
/// also keeps a resumed session from claiming the note it left itself when
/// it ended, which would take it from whoever starts next.
///
/// A note that is waiting and old news is left waiting: it may be news to
/// the next session that starts fresh.
pub(crate) fn handed(
    store: &Store,
    scope: &ResolvedScope,
    claimant: SessionId,
    slot: &Slot,
    swept: Swept,
    now: Timestamp,
) -> Result<Option<String>, WebError> {
    let known = store.last_work(claimant, now)?;
    let waiting_is_old_news = store
        .latest_handoff(scope.project_id, slot)?
        .is_some_and(|note| {
            note.taken_by.is_none() && !note.dropped && old_news(note.as_of, known)
        });
    if !waiting_is_old_news
        && let Some(note) = store.claim_handoff(scope.project_id, claimant, slot, now)?
    {
        return Ok(Some(note));
    }
    hand_on(store, scope, claimant, slot, swept, known)
}

/// Whether a note describing work up to `as_of` tells a session that already
/// knows its own work up to `known` nothing new.
fn old_news(as_of: Timestamp, known: Option<Timestamp>) -> bool {
    known.is_some_and(|known| as_of <= known)
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

    fn sweep(harness: &Harness, claimant: SessionId, embed_model: Option<&str>) -> bool {
        hand_over_peers(
            &harness.state.store,
            &harness.state.wiki,
            embed_model,
            &harness.scope,
            claimant,
            &slot(),
            now(),
        )
        .written
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

        assert!(sweep(&harness, new, None));

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

        assert!(!sweep(&harness, new, None));

        assert!(claimed(&harness, new).is_none());
        assert_eq!(state_of(&harness, quiet), "open");
    }

    #[test]
    fn a_session_does_not_write_itself_up() {
        let harness = harness();
        let id = session_quiet_for(&harness, "the-only-one", 600);

        assert!(!sweep(&harness, id, None));
        assert!(claimed(&harness, id).is_none());
    }

    #[test]
    fn a_project_can_turn_it_off() {
        let harness = harness_with("[sessions]\nhandover_after_seconds = 0\n");
        session_quiet_for(&harness, "the-closed-terminal", 60 * 60 * 24);
        let new = starting(&harness);

        assert!(!sweep(&harness, new, None));
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

        assert!(sweep(&harness, new, None));

        let note = claimed(&harness, new).expect("a note to take");
        assert!(note.contains("make newer pass"), "{note}");
    }

    /// A session that ended `seconds` ago and left its note as it did.
    fn with_note_from(harness: &Harness, name: &str, seconds: i64) -> SessionId {
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
        let store = &harness.state.store;
        store.ensure_session(&session).expect("session");
        store
            .close_session(id, seconds_ago(seconds))
            .expect("close");
        harness
            .state
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                harness.scope.project_id,
                id,
                slot(),
                &format!("the note {name} left"),
                seconds_ago(seconds),
            ))
            .expect("handoff");
        id
    }

    /// What happened live: a Codex terminal was left open, a Claude session
    /// beside it then ended and left its note, and the Codex opened after that
    /// was handed the older terminal instead. The session that ended last is
    /// the one the person just closed.
    #[test]
    fn a_note_written_after_the_silence_is_not_superseded() {
        let harness = harness();
        session_quiet_for(&harness, "the-older-terminal", 300);
        with_note_from(&harness, "the-session-that-ended", 60);
        let new = starting(&harness);

        assert!(!sweep(&harness, new, None));

        let note = claimed(&harness, new).expect("a note to take");
        assert!(note.contains("the-session-that-ended"), "{note}");
    }

    /// The other way round the quiet session is the newer news, and a stale
    /// note from before it must not keep it from being handed over.
    #[test]
    fn a_note_older_than_the_silence_still_gives_way() {
        let harness = harness();
        with_note_from(&harness, "yesterday", 3_600);
        session_quiet_for(&harness, "the-closed-terminal", 180);
        let new = starting(&harness);

        assert!(sweep(&harness, new, None));

        let note = claimed(&harness, new).expect("a note to take");
        assert!(note.contains("make the-closed-terminal pass"), "{note}");
    }

    /// A third terminal opened beside a session that was already handed over
    /// does not write it up a second time: the session has not moved since,
    /// so there is no new page to commit. What the third is told is
    /// [`hand_on`]'s business, below.
    #[test]
    fn a_quiet_session_is_written_up_once() {
        let harness = harness();
        session_quiet_for(&harness, "the-closed-terminal", 180);
        let second = starting(&harness);
        assert!(sweep(&harness, second, None));
        assert!(claimed(&harness, second).is_some());

        let third = starting_as(&harness, "the-third-one");
        assert!(!sweep(&harness, third, None));
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
        assert!(sweep(&harness, third, None));
        let note = claimed(&harness, third).expect("a note to take");
        assert!(note.contains("exporter"), "{note}");
    }

    /// An open session that was asked something `asked` seconds ago and
    /// answered `answered` seconds ago, and has been waiting for its person
    /// since.
    fn answered_session(
        harness: &Harness,
        name: &str,
        asked: i64,
        answered: i64,
        answer: &str,
    ) -> SessionId {
        let id = SessionId::derive(harness.scope.project_id, name);
        let session = new_session(
            id,
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::Codex,
            harness.scope.root.clone(),
            seconds_ago(asked + 5),
            None,
        );
        let store = &harness.state.store;
        store.ensure_session(&session).expect("session");
        for (kind, body, at) in [
            (
                EventKind::UserPrompt,
                format!("what did {name} leave?"),
                asked,
            ),
            (EventKind::AssistantMessage, answer.to_owned(), answered),
        ] {
            store
                .insert_observation(&new_observation(
                    id,
                    kind,
                    None,
                    BoundedBody::truncating(body, 1024),
                    seconds_ago(at),
                ))
                .expect("observation");
        }
        id
    }

    /// What the web handler does for a session that has just started: write
    /// up whoever is beside it, claim what is waiting, and hand on the slot's
    /// newest note when nothing is.
    fn arrives(harness: &Harness, claimant: SessionId) -> Option<String> {
        let swept = hand_over_peers(
            &harness.state.store,
            &harness.state.wiki,
            None,
            &harness.scope,
            claimant,
            &slot(),
            now(),
        );
        handed(
            &harness.state.store,
            &harness.scope,
            claimant,
            &slot(),
            swept,
            now(),
        )
        .expect("handed")
    }

    /// The claimant has done something `seconds` ago, as a resumed session has.
    fn did_something(harness: &Harness, id: SessionId, seconds: i64) {
        harness
            .state
            .store
            .insert_observation(&new_observation(
                id,
                EventKind::AssistantMessage,
                None,
                BoundedBody::truncating("released and installed", 1024),
                seconds_ago(seconds),
            ))
            .expect("observation");
    }

    /// What happened on 2026-09-26, with the release's own note out of the
    /// way: a Codex terminal answered and was left open, the Claude session
    /// that took over from it did an hour's more work, and the Codex terminal
    /// ended long after. Resumed, the Claude session is not handed the Codex
    /// note: it knows more than it says.
    #[test]
    fn a_resumed_session_is_not_handed_a_note_older_than_its_own_work() {
        let harness = harness();
        let codex = answered_session(
            &harness,
            "codex-left-open",
            7_300,
            7_200,
            "e684e95 is the last commit",
        );
        let claude = starting_as(&harness, "claude-that-released");
        did_something(&harness, claude, 3_600);
        harness
            .state
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                harness.scope.project_id,
                codex,
                slot(),
                "the note codex-left-open left: change nothing",
                seconds_ago(60),
            ))
            .expect("codex's note");

        assert_eq!(arrives(&harness, claude), None);
        assert!(
            harness
                .state
                .store
                .peek_handoff(harness.scope.project_id, &slot())
                .expect("peek")
                .is_some(),
            "left waiting for a session to which it is news"
        );
    }

    /// The same, the way `claude --resume` does it: a session that lives for
    /// ten seconds starts first and takes the note, and the resumed one is
    /// asked second, which is when a note is handed on.
    #[test]
    fn a_resumed_session_is_not_handed_on_a_note_older_than_its_own_work() {
        let harness = harness();
        let codex = answered_session(
            &harness,
            "codex-left-open",
            7_300,
            7_200,
            "e684e95 is the last commit",
        );
        let claude = starting_as(&harness, "claude-that-released");
        did_something(&harness, claude, 3_600);
        harness
            .state
            .store
            .close_session(claude, seconds_ago(3_000))
            .expect("ended");
        harness
            .state
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                harness.scope.project_id,
                codex,
                slot(),
                "the note codex-left-open left: change nothing",
                seconds_ago(60),
            ))
            .expect("codex's note");
        let fleeting = starting_as(&harness, "claude-resume-starts-this-first");
        assert!(
            arrives(&harness, fleeting).is_some(),
            "news to a new session"
        );

        assert_eq!(arrives(&harness, claude), None);
    }

    /// Resumed after somebody else worked, a session is handed what they did.
    #[test]
    fn a_resumed_session_is_handed_what_happened_while_it_was_away() {
        let harness = harness();
        let claude = starting_as(&harness, "claude-that-went-away");
        did_something(&harness, claude, 7_200);
        harness
            .state
            .store
            .close_session(claude, seconds_ago(7_000))
            .expect("ended");
        let codex = answered_session(
            &harness,
            "codex-worked-since",
            3_700,
            3_600,
            "the exporter passes",
        );
        harness
            .state
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                harness.scope.project_id,
                codex,
                slot(),
                "the note codex-worked-since left",
                seconds_ago(60),
            ))
            .expect("codex's note");
        let fleeting = starting_as(&harness, "claude-resume-starts-this-first");
        assert!(arrives(&harness, fleeting).is_some());

        // Resuming is recorded before the handoff is asked for, and it is not
        // work: counted as such, every note would be older than it.
        harness
            .state
            .store
            .insert_observation(&new_observation(
                claude,
                EventKind::SessionStart,
                None,
                BoundedBody::truncating("resume", 1024),
                seconds_ago(1),
            ))
            .expect("resumed");

        let told = arrives(&harness, claude).expect("the resumed session is told");
        assert!(told.contains("codex-worked-since"), "{told}");
    }

    /// A session that ended and is resumed does not claim its own note, which
    /// the next session to start is owed.
    #[test]
    fn a_resumed_session_does_not_take_its_own_note() {
        let harness = harness();
        let claude = starting_as(&harness, "claude-that-ended");
        did_something(&harness, claude, 600);
        harness
            .state
            .store
            .close_session(claude, seconds_ago(500))
            .expect("ended");
        harness
            .state
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                harness.scope.project_id,
                claude,
                slot(),
                "the note claude-that-ended left",
                seconds_ago(300),
            ))
            .expect("its own note");

        assert_eq!(arrives(&harness, claude), None);
        let next = starting_as(&harness, "codex-next");
        let told = arrives(&harness, next).expect("the next session is told");
        assert!(told.contains("claude-that-ended"), "{told}");
    }

    /// The sequence that failed live on 2026-09-23, step by step. A Codex
    /// terminal was left open and quiet; a Claude session did the real work
    /// and ended; the Codex opened next was told about that work, answered,
    /// and was left open in turn; the Claude opened a minute later was handed
    /// the Codex terminal from thirty-five minutes before, not the one it had
    /// just been looking at.
    #[test]
    fn each_agent_is_handed_the_newest_work_not_the_oldest_open_terminal() {
        let harness = harness();
        session_quiet_for(&harness, "codex-left-open", 35 * 60);
        with_note_from(&harness, "claude-that-deployed", 90);

        let codex = starting_as(&harness, "codex-next");
        let told = arrives(&harness, codex).expect("codex is told something");
        assert!(told.contains("claude-that-deployed"), "{told}");

        answered_session(
            &harness,
            "codex-that-answered",
            60,
            47,
            "the fix is live and the server runs 1.2.1",
        );

        let claude = starting_as(&harness, "claude-after");
        let told = arrives(&harness, claude).expect("claude is told something");
        assert!(told.contains("the fix is live"), "{told}");
        assert!(!told.contains("codex-left-open"), "{told}");
    }

    /// The same sequence, with the Codex in between still mid-turn: nothing
    /// newer than the ended session's note can be written up, and the terminal
    /// left open half an hour earlier must not be written up in its place just
    /// because the note has already been taken.
    #[test]
    fn a_terminal_left_open_does_not_outrank_a_note_already_taken() {
        let harness = harness();
        session_quiet_for(&harness, "codex-left-open", 35 * 60);
        with_note_from(&harness, "claude-that-deployed", 90);
        let codex = starting_as(&harness, "codex-next");
        assert!(arrives(&harness, codex).is_some());

        let claude = starting_as(&harness, "claude-after");
        let told = arrives(&harness, claude).expect("claude is told something");

        assert!(told.contains("claude-that-deployed"), "{told}");
        assert!(!told.contains("codex-left-open"), "{told}");
    }

    /// A terminal closed mid-turn is not a candidate until its silence is
    /// long enough, and an older session written up meanwhile must not bury
    /// it: that note is fresh ink on old work, and the closed terminal's work
    /// is newer than what it describes.
    #[test]
    fn a_terminal_closed_mid_turn_is_not_buried_by_an_older_write_up() {
        let harness = harness();
        answered_session(&harness, "answered-earlier", 900, 600, "the older answer");
        session_quiet_for(&harness, "closed-mid-turn", 30);

        let first = starting_as(&harness, "first-to-arrive");
        let told = arrives(&harness, first).expect("told about the older answer");
        assert!(told.contains("the older answer"), "{told}");

        let later = now() + jiff::Span::new().seconds(200);
        let second = starting_as(&harness, "second-to-arrive");
        let swept = hand_over_peers(
            &harness.state.store,
            &harness.state.wiki,
            None,
            &harness.scope,
            second,
            &slot(),
            later,
        );
        assert!(swept.written, "{swept:?}");
        let told = harness
            .state
            .store
            .claim_handoff(harness.scope.project_id, second, &slot(), later)
            .expect("claim")
            .expect("a note to take");
        assert!(told.contains("make closed-mid-turn pass"), "{told}");
    }

    /// A session whose last word was its answer is waiting for its person, not
    /// working, and the person who read that answer and switched agents is
    /// carrying on from it — however few seconds ago it was.
    #[test]
    fn a_session_that_has_answered_is_handed_over_at_once() {
        let harness = harness();
        answered_session(
            &harness,
            "just-answered",
            25,
            20,
            "the parser now keeps comments",
        );
        let new = starting(&harness);

        assert!(sweep(&harness, new, None));

        let note = claimed(&harness, new).expect("a note to take");
        assert!(note.contains("the parser now keeps comments"), "{note}");
        assert!(note.contains("still open"), "{note}");
    }

    /// Claude ends, Codex opens and takes the note, the person closes Codex
    /// having done nothing and opens Claude again. Nothing has happened since
    /// the note, so the note is still where things stand.
    #[test]
    fn a_taken_note_is_handed_on_while_nothing_has_happened_since() {
        let harness = harness();
        with_note_from(&harness, "claude-that-ended", 120);

        let codex = starting_as(&harness, "codex-opened-and-closed");
        let first = arrives(&harness, codex).expect("codex is told");
        let claude = starting_as(&harness, "claude-again");
        let second = arrives(&harness, claude).expect("claude is told the same");

        assert_eq!(first, second);
        assert!(second.contains("claude-that-ended"), "{second}");
    }

    /// Handed on to somebody new, not back to whoever already has it or wrote
    /// it.
    #[test]
    fn a_note_is_not_handed_back_to_the_session_that_has_it() {
        let harness = harness();
        let author = with_note_from(&harness, "claude-that-ended", 120);
        let codex = starting_as(&harness, "codex");
        assert!(arrives(&harness, codex).is_some());

        assert!(arrives(&harness, codex).is_none(), "it already took it");
        assert!(arrives(&harness, author).is_none(), "it wrote it");
    }

    /// Somebody decided the next session is better off without it.
    #[test]
    fn a_dropped_note_is_not_handed_on() {
        let harness = harness();
        with_note_from(&harness, "a-bad-summary", 120);
        harness
            .state
            .store
            .discard_handoff(harness.scope.project_id, &slot())
            .expect("discard");

        let new = starting(&harness);
        assert!(arrives(&harness, new).is_none());
    }

    /// When newer work exists and could not be written up, the older note is
    /// not the latest account of the slot, and handing it on would be the
    /// mistake this module was written to stop. Silence is the honest answer.
    #[test]
    fn an_older_note_is_not_handed_on_in_front_of_newer_work() {
        let harness = harness();
        with_note_from(&harness, "claude-that-ended", 600);
        let codex = starting_as(&harness, "codex");
        assert!(arrives(&harness, codex).is_some());
        answered_session(&harness, "codex-that-worked", 120, 60, "newer work");

        let claude = starting_as(&harness, "claude");
        let busy = harness.state.wiki.lock();
        let swept = hand_over_peers(
            &harness.state.store,
            &harness.state.wiki,
            None,
            &harness.scope,
            claude,
            &slot(),
            now(),
        );
        drop(busy);

        assert!(swept.unsettled);
        assert!(claimed(&harness, claude).is_none());
        assert!(
            hand_on(
                &harness.state.store,
                &harness.scope,
                claude,
                &slot(),
                swept,
                None
            )
            .expect("hand on")
            .is_none()
        );
    }

    /// The newest session beside the new one had nothing worth a page — it
    /// was opened and closed again — so the one before it is written up.
    #[test]
    fn a_session_with_nothing_to_say_gives_way_to_the_one_before_it() {
        let harness = harness();
        answered_session(&harness, "did-the-work", 300, 240, "the exporter passes");
        let empty = SessionId::derive(harness.scope.project_id, "opened-and-closed");
        harness
            .state
            .store
            .ensure_session(&new_session(
                empty,
                harness.scope.project_id,
                harness.scope.workspace_id,
                AgentKind::Codex,
                harness.scope.root.clone(),
                seconds_ago(200),
                None,
            ))
            .expect("session");
        harness
            .state
            .store
            .insert_observation(&new_observation(
                empty,
                EventKind::SessionStart,
                None,
                BoundedBody::truncating("startup", 1024),
                seconds_ago(200),
            ))
            .expect("observation");
        let new = starting_as(&harness, "the-new-one");

        let told = arrives(&harness, new).expect("told about the work");
        assert!(told.contains("the exporter passes"), "{told}");
    }
}
