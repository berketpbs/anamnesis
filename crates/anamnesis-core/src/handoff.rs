//! Handoffs delivered from one session to the next.

use jiff::Timestamp;

use crate::ids::{HandoffId, ProjectId, SessionId, WorkstreamId};
use crate::observation::BoundedBody;
use crate::scope::OperatorName;

/// Which pending-handoff slot a note belongs in.
///
/// A project holds at most one pending handoff per slot, and the slot is what
/// decides whether two sessions are continuing the same thread of work or two
/// different ones. Both keys default to absent, which is one slot for the whole
/// project — the behaviour a single person on a single machine has always had,
/// and the one every setting here is a departure from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Slot {
    /// The workstream this slot belongs to, if any.
    ///
    /// `None` is the shared, workstream-less slot. Named threads of work get
    /// one each, so finishing a session on one does not hand its note to a
    /// session resuming the other.
    pub workstream_id: Option<WorkstreamId>,

    /// The operator this slot belongs to, when the project keys slots by
    /// operator and the caller was one the server could name.
    ///
    /// `None` is the shared slot again, and is what every anonymous caller
    /// gets. Set only where `[slots] per_user` is on: keying by an operator
    /// the project never asked to separate would split one person's memory in
    /// half the first time they used a second token.
    pub operator: Option<OperatorName>,
}

impl Slot {
    /// The one slot a project has when it has asked for nothing else.
    pub fn shared() -> Self {
        Self::default()
    }

    /// The slot belonging to a workstream.
    pub fn for_workstream(workstream_id: Option<WorkstreamId>) -> Self {
        Self {
            workstream_id,
            operator: None,
        }
    }

    /// The same slot, narrowed to one operator.
    pub fn for_operator(mut self, operator: Option<OperatorName>) -> Self {
        self.operator = operator;
        self
    }

    /// The workstream key as SQL sees it.
    pub fn workstream_key(&self) -> Option<String> {
        self.workstream_id.map(|id| id.to_string())
    }

    /// The operator key as SQL sees it.
    pub fn operator_key(&self) -> Option<String> {
        self.operator.as_ref().map(ToString::to_string)
    }
}

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
    /// The operator this handoff's slot is keyed to, when the project keys
    /// slots by operator.
    ///
    /// Unlike the operator recorded on a session, this is not provenance: it
    /// is half the slot key, and it is `None` for a project that has not
    /// asked for per-operator slots however the session was authenticated.
    #[serde(default)]
    pub operator: Option<OperatorName>,
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
            operator: None,
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
