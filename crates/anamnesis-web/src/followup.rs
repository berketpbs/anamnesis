//! What a session is owed about the one it took over from, after it started.
//!
//! A starting session is handed the note the session before it left, and the
//! decisions the project holds, once, at its start. The model's note for a
//! session that has just ended is not ready then if the person switched at
//! once: on this project's own memory it landed a median of 22 seconds after
//! the counted note (90th percentile 46, slowest 136, over 37 sessions), and 6
//! of 32 switches between agents began within 30 seconds of the session before
//! ending. Those sessions were handed the counted note — the last request and
//! the last answer, each cut at 400 characters — and the model's note, arriving
//! a few seconds later, was dropped, because a note that has been taken must
//! not be handed to the *next* session as though nothing happened since. The
//! decisions the same pass wrote reached them only if their prompt happened to
//! look like one.
//!
//! The same is true of a terminal closed without an end: the next session is
//! handed a write-up of it straight away, and the decisions it held are written
//! by [`crate::settle`] two minutes into its silence, after that session has
//! already started.
//!
//! Dropping the note was right for everyone after the session that took over,
//! and wrong for that one session. So what arrives within [`WINDOW`] of a
//! takeover is kept for it — the note, the decisions written with it, or both
//! — and handed over with its next prompt, the first moment after its start
//! that anything can be put in front of the agent.

use anamnesis_core::audit::{Action, AuditEntry, Via};
use anamnesis_core::brief::{self, Followed, Standing};
use anamnesis_core::ids::SessionId;
use anamnesis_core::scope::ResolvedScope;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::WebError;

/// How long after a takeover what arrives about the session taken over from
/// is still news to the one that took over.
///
/// Wide against the measurements above, on purpose: a model chain that falls
/// back to a local model takes minutes, and a note that misses the window is
/// lost for good. Narrow against a working session: ten minutes in, the person
/// has moved on, and an account of what came before is old news rather than
/// the thing they switched to carry on from.
pub(crate) const WINDOW_SECONDS: i64 = 10 * 60;

/// Leave the session that took over from `from_session` what was just written
/// about it, if one did, is still open, and took over within [`WINDOW_SECONDS`].
///
/// Whether anything was left is returned for the log and the tests; a failure
/// costs the follow-up and nothing else, and the caller treats it that way.
pub(crate) fn follow_up(
    store: &Store,
    from_session: SessionId,
    note: Option<&str>,
    notes: &[String],
    now: Timestamp,
) -> Result<bool, WebError> {
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    if note.is_none() && notes.is_empty() {
        return Ok(false);
    }
    let Some(takeover) = store.open_taker_of(from_session)? else {
        return Ok(false);
    };
    if takeover.to_session == from_session
        || now.as_second() - takeover.at.as_second() > WINDOW_SECONDS
    {
        return Ok(false);
    }
    store.record_followup(from_session, takeover.to_session, note, notes, now)?;
    tracing::info!(
        from = %from_session,
        to = %takeover.to_session,
        note = note.is_some(),
        notes = notes.len(),
        "left the session that took over what arrived after it started"
    );
    Ok(true)
}

/// Everything owed to the session a harness calls `asking`, rendered for the
/// prompt it just sent, and marked as handed over.
///
/// One block per session taken over from, which in practice is one: the
/// newest note in it, and the decisions among everything written for it that
/// still stand. Empty when nothing was owed.
///
/// Only taking them can fail. Once they are taken, a decision that cannot be
/// looked up or a session that cannot be named costs its line and not the
/// follow-up, which would otherwise be marked handed over and lost.
pub(crate) fn deliver(
    store: &Store,
    scope: &ResolvedScope,
    asking: &str,
    now: Timestamp,
) -> Result<String, WebError> {
    let session = SessionId::derive(scope.project_id, asking);
    let owed = store.take_followups(session, now)?;
    if owed.is_empty() {
        return Ok(String::new());
    }

    let mut from: Vec<SessionId> = Vec::new();
    for followup in &owed {
        if !from.contains(&followup.from_session) {
            from.push(followup.from_session);
        }
    }

    let mut blocks: Vec<String> = Vec::new();
    for source in from {
        let mut note: Option<String> = None;
        let mut notes: Vec<String> = Vec::new();
        for followup in owed
            .iter()
            .filter(|followup| followup.from_session == source)
        {
            if followup.body.is_some() {
                note.clone_from(&followup.body);
            }
            for path in &followup.notes {
                if !notes.contains(path) {
                    notes.push(path.clone());
                }
            }
        }

        let decisions = store
            .standing_among(scope.project_id, &notes)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not look up the decisions in a follow-up");
                Vec::new()
            })
            .into_iter()
            .map(|decision| Standing {
                path: decision.path,
                title: decision.title,
                as_of: decision.updated_at.to_string().chars().take(10).collect(),
                source_session: decision
                    .source_session
                    .map(|session| session.to_string().chars().take(8).collect()),
                origin: decision.origin,
            })
            .collect::<Vec<_>>();
        let named = match store.load_session(source).ok().flatten() {
            Some(earlier) => format!("{}, started {}", earlier.agent, earlier.started_at),
            None => format!(
                "session {}",
                source.to_string().chars().take(8).collect::<String>()
            ),
        };

        let block = brief::followed(
            &Followed {
                from: named,
                note,
                decisions,
            },
            &scope.recall,
        );
        if !block.is_empty() {
            blocks.push(block);
        }

        let short: String = source.to_string().chars().take(8).collect();
        let entry = AuditEntry::new(Action::HandoffClaimed, Via::Http, asking, now)
            .in_project(scope.project_id)
            .saying(format!(
                "what arrived about session {short} after this one took over from it"
            ));
        if let Err(error) = store.append_audit(&entry) {
            tracing::warn!(%error, "a follow-up was handed over but not recorded in the audit log");
        }
    }
    Ok(blocks.join("\n"))
}
