//! Who changed memory, and what they changed.
//!
//! Capture already records what happened *inside* sessions: every prompt and
//! tool call is an observation, and the transcript outlives the index. This is
//! the other question, and one nothing could answer until now — who reached in
//! and changed the memory itself. A page rewritten by hand, a session
//! forgotten, a handoff claimed, a proposal applied: each of those replaces or
//! removes something a later session would otherwise have been told, and until
//! there is a line saying so, the only evidence is that memory now says
//! something different.
//!
//! Two rules keep it worth reading.
//!
//! **Only deliberate changes.** An event arriving from a hook is not audited —
//! the observations table *is* that record, and duplicating it would bury the
//! handful of lines that matter under thousands that do not. What is audited
//! is what a person or an agent asked for.
//!
//! **It outlives what it describes.** An audit line about a page that has been
//! forgotten is exactly the line somebody needs afterwards, so nothing here
//! cascades from the rows it refers to. The subject is recorded as text, not
//! as a foreign key.

use crate::ids::{AuditId, ProjectId};
use crate::scope::OperatorName;
use jiff::Timestamp;

/// What was done.
///
/// Deliberately coarse: the useful question is "what changed and who changed
/// it", and a taxonomy fine enough to distinguish every code path would be a
/// taxonomy nobody keeps accurate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    /// A page was written or rewritten by hand.
    PageWritten,
    /// A page was removed on purpose.
    PageForgotten,
    /// A page was removed by the decay sweep.
    PageSwept,
    /// A page was moved to another tier, or otherwise reshaped by a proposal.
    PagePromoted,
    /// A session, its observations, and its transcript were removed.
    SessionForgotten,
    /// A waiting handoff was taken by a session.
    HandoffClaimed,
    /// A waiting handoff was thrown away unread.
    HandoffDiscarded,
    /// A proposal was dismissed rather than carried out.
    ProposalDismissed,
    /// Memory was restored from an archive.
    Restored,
    /// A project's memory was removed in its entirety.
    Purged,
    /// A project's memory was moved to a new identity.
    Renamed,
}

impl Action {
    /// Canonical identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PageWritten => "page.written",
            Self::PageForgotten => "page.forgotten",
            Self::PageSwept => "page.swept",
            Self::PagePromoted => "page.promoted",
            Self::SessionForgotten => "session.forgotten",
            Self::HandoffClaimed => "handoff.claimed",
            Self::HandoffDiscarded => "handoff.discarded",
            Self::ProposalDismissed => "proposal.dismissed",
            Self::Restored => "memory.restored",
            Self::Purged => "memory.purged",
            Self::Renamed => "memory.renamed",
        }
    }

    /// Recover an action from its stored form.
    ///
    /// An unrecognised value keeps its own name rather than being defaulted:
    /// an audit line from a newer build says what it says, and rewriting it as
    /// something else would be this log telling its own kind of lie.
    pub fn from_storage(raw: &str) -> Result<Self, String> {
        Ok(match raw {
            "page.written" => Self::PageWritten,
            "page.forgotten" => Self::PageForgotten,
            "page.swept" => Self::PageSwept,
            "page.promoted" => Self::PagePromoted,
            "session.forgotten" => Self::SessionForgotten,
            "handoff.claimed" => Self::HandoffClaimed,
            "handoff.discarded" => Self::HandoffDiscarded,
            "proposal.dismissed" => Self::ProposalDismissed,
            "memory.restored" => Self::Restored,
            "memory.purged" => Self::Purged,
            "memory.renamed" => Self::Renamed,
            other => return Err(other.to_owned()),
        })
    }

    /// How it reads in a listing.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::PageWritten => "wrote",
            Self::PageForgotten => "forgot",
            Self::PageSwept => "swept",
            Self::PagePromoted => "promoted",
            Self::SessionForgotten => "forgot session",
            Self::HandoffClaimed => "claimed handoff",
            Self::HandoffDiscarded => "discarded handoff",
            Self::ProposalDismissed => "dismissed proposal",
            Self::Restored => "restored memory",
            Self::Purged => "purged the memory of",
            Self::Renamed => "renamed",
        }
    }
}

/// Which door the change came through.
///
/// Not decoration: the same action means different things depending on it. A
/// page written over MCP was written by a model mid-session; the same page
/// written through the CLI was written by a person who meant to. And on a
/// server more than one machine can reach, `Cli` means "someone with the
/// disk", which is a different kind of access from a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Via {
    /// A command run against the data directory.
    Cli,
    /// An agent's tool call.
    Mcp,
    /// The HTTP surface.
    Http,
    /// The server acting on its own schedule.
    Server,
}

impl Via {
    /// Canonical identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
            Self::Http => "http",
            Self::Server => "server",
        }
    }

    /// Recover a door from its stored form, defaulting to the one that cannot
    /// be attributed: a line whose origin this build does not recognise is
    /// still a line about a real change.
    pub fn from_storage(raw: &str) -> Self {
        match raw {
            "mcp" => Self::Mcp,
            "http" => Self::Http,
            "server" => Self::Server,
            _ => Self::Cli,
        }
    }
}

/// One line of the audit log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditEntry {
    /// Minted, time-ordered identifier.
    pub id: AuditId,
    /// When it happened.
    pub at: Timestamp,
    /// Which project's memory changed, when the change belonged to one.
    pub project_id: Option<ProjectId>,
    /// Who did it, when the server could tell.
    ///
    /// `None` on every single-user install, where nothing is asked for a
    /// token and stamping "unknown" on every line would be noise standing in
    /// for a fact nobody was looking for.
    pub operator: Option<OperatorName>,
    /// Which door it came through.
    pub via: Via,
    /// What was done.
    pub action: Action,
    /// What it was done to: a page path, a session id, a proposal.
    pub subject: String,
    /// Anything else worth keeping, in a person's words.
    pub detail: Option<String>,
}

impl AuditEntry {
    /// A new line, with the identifier and the time filled in.
    pub fn new(action: Action, via: Via, subject: impl Into<String>, at: Timestamp) -> Self {
        Self {
            id: AuditId::new(),
            at,
            project_id: None,
            operator: None,
            via,
            action,
            subject: subject.into(),
            detail: None,
        }
    }

    /// Attribute it to a project.
    #[must_use]
    pub fn in_project(mut self, project_id: ProjectId) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Attribute it to whoever the server could name.
    #[must_use]
    pub fn by(mut self, operator: Option<OperatorName>) -> Self {
        self.operator = operator;
        self
    }

    /// Say more about it.
    #[must_use]
    pub fn saying(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The line as somebody reads it, without the timestamp.
    pub fn summary(&self) -> String {
        let who = match &self.operator {
            Some(operator) => operator.to_string(),
            None => "someone unnamed".to_owned(),
        };
        let detail = match &self.detail {
            Some(detail) => format!(" — {detail}"),
            None => String::new(),
        };
        format!(
            "{who} ({}) {} {}{detail}",
            self.via.as_str(),
            self.action.describe(),
            self.subject
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_survives_a_round_trip_through_storage() {
        for action in [
            Action::PageWritten,
            Action::PageForgotten,
            Action::PageSwept,
            Action::PagePromoted,
            Action::SessionForgotten,
            Action::HandoffClaimed,
            Action::HandoffDiscarded,
            Action::ProposalDismissed,
            Action::Restored,
            Action::Purged,
            Action::Renamed,
        ] {
            assert_eq!(
                Action::from_storage(action.as_str()),
                Ok(action),
                "{}",
                action.as_str()
            );
        }
    }

    /// A line written by a newer build is not rewritten as something this one
    /// happens to know. An audit log that quietly relabels its own entries is
    /// worse than one with a gap in it.
    #[test]
    fn an_action_this_build_does_not_know_keeps_its_name() {
        assert_eq!(
            Action::from_storage("page.rewrapped"),
            Err("page.rewrapped".to_owned())
        );
    }

    #[test]
    fn a_door_this_build_does_not_know_is_still_a_change() {
        assert_eq!(Via::from_storage("mcp"), Via::Mcp);
        assert_eq!(Via::from_storage("something-new"), Via::Cli);
    }

    /// The summary is the whole product for a person reading `anamnesis
    /// audit`, so it has to name all four things: who, how, what, and to what.
    #[test]
    fn a_line_names_who_did_what_through_which_door() {
        let at: Timestamp = "2026-09-01T12:00:00Z".parse().expect("time");
        let entry = AuditEntry::new(Action::PageForgotten, Via::Cli, "notes/api.md", at)
            .by(Some(
                OperatorName::sanitized("alice").expect("operator name"),
            ))
            .saying("2 pages, commit ab12cd34");

        let line = entry.summary();
        assert!(line.contains("alice"), "{line}");
        assert!(line.contains("cli"), "{line}");
        assert!(line.contains("forgot"), "{line}");
        assert!(line.contains("notes/api.md"), "{line}");
        assert!(line.contains("commit ab12cd34"), "{line}");
    }

    /// Every single-user install has no operator, and saying "unknown" on
    /// every line would put noise where a fact nobody asked for would go.
    #[test]
    fn an_unattributed_line_says_so_without_inventing_a_name() {
        let at: Timestamp = "2026-09-01T12:00:00Z".parse().expect("time");
        let line = AuditEntry::new(Action::PageWritten, Via::Mcp, "notes/api.md", at).summary();

        assert!(line.contains("someone unnamed"), "{line}");
        assert!(!line.contains("unknown"), "{line}");
    }
}
