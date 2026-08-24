//! Handoffs delivered from one session to the next.

use jiff::Timestamp;

use crate::ids::{HandoffId, ProjectId, SessionId, WorkstreamId};
use crate::observation::BoundedBody;

/// Delivery state of a handoff.
///
/// A handoff is single-use: the first session to accept it consumes it, so two
/// agents starting concurrently in the same project cannot both act on the same
/// "here is where I left off" note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HandoffState {
    /// Written and waiting to be picked up.
    Pending,
    /// Consumed by a session.
    Accepted,
    /// Superseded by a newer handoff before anyone read it.
    Expired,
}

/// A bounded summary passed to whichever session starts next.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Handoff {
    /// Minted, time-ordered identifier.
    pub id: HandoffId,
    /// Project the handoff belongs to.
    pub project_id: ProjectId,
    /// The workstream this handoff's pending slot is keyed to, if any.
    ///
    /// `None` shares one slot with every other workstream-less handoff in
    /// the project — today's behaviour. A workstream's handoffs are keyed to
    /// its own slot, so claiming one never consumes another's.
    pub workstream_id: Option<WorkstreamId>,
    /// Session that produced it.
    pub from_session: SessionId,
    /// Session that consumed it, once accepted.
    pub to_session: Option<SessionId>,
    /// The summary itself.
    pub body: BoundedBody,
    /// When it was written.
    pub created_at: Timestamp,
    /// When it was consumed.
    pub accepted_at: Option<Timestamp>,
    /// Current delivery state.
    pub state: HandoffState,
}

impl Handoff {
    /// Whether this handoff is still available to be claimed.
    pub fn is_claimable(&self) -> bool {
        matches!(self.state, HandoffState::Pending)
    }

    /// Mark the handoff as consumed by `session` at `at`.
    ///
    /// Returns `false` when it was already claimed or expired, leaving the
    /// handoff untouched.
    pub fn accept(&mut self, session: SessionId, at: Timestamp) -> bool {
        if !self.is_claimable() {
            return false;
        }
        self.state = HandoffState::Accepted;
        self.to_session = Some(session);
        self.accepted_at = Some(at);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> Handoff {
        Handoff {
            id: HandoffId::new(),
            project_id: ProjectId::from_uuid(uuid::Uuid::nil()),
            workstream_id: None,
            from_session: SessionId::new(),
            to_session: None,
            body: BoundedBody::truncating("carry on", BoundedBody::DEFAULT_LIMIT),
            created_at: Timestamp::now(),
            accepted_at: None,
            state: HandoffState::Pending,
        }
    }

    #[test]
    fn first_accept_wins() {
        let mut handoff = pending();
        let first = SessionId::new();
        let second = SessionId::new();

        assert!(handoff.accept(first, Timestamp::now()));
        assert!(!handoff.accept(second, Timestamp::now()));
        assert_eq!(handoff.to_session, Some(first));
        assert_eq!(handoff.state, HandoffState::Accepted);
    }

    #[test]
    fn expired_handoffs_cannot_be_claimed() {
        let mut handoff = pending();
        handoff.state = HandoffState::Expired;
        assert!(!handoff.accept(SessionId::new(), Timestamp::now()));
        assert!(handoff.to_session.is_none());
    }
}
