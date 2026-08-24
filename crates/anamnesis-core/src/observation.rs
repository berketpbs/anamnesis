//! Captured lifecycle events.

use jiff::Timestamp;

use crate::error::{CoreError, Result};
use crate::ids::{ObservationId, SessionId};

/// Lifecycle boundary an observation was captured at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// Session opened.
    SessionStart,
    /// A prompt written by the operator.
    UserPrompt,
    /// A tool invocation and its outcome.
    ToolUse,
    /// Checkpoint taken before the model compacts its context.
    PreCompact,
    /// Summary produced by compaction.
    PostCompact,
    /// Session closed.
    SessionEnd,
    /// Out-of-band notice from the harness.
    Notification,
}

impl EventKind {
    /// Canonical lowercase identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::UserPrompt => "user-prompt",
            Self::ToolUse => "tool-use",
            Self::PreCompact => "pre-compact",
            Self::PostCompact => "post-compact",
            Self::SessionEnd => "session-end",
            Self::Notification => "notification",
        }
    }

    /// Recover a kind from its stored form, the inverse of [`Self::as_str`].
    ///
    /// Anything unrecognised becomes [`Self::Notification`] rather than an
    /// error: a row written by a newer version naming an event this build has
    /// never heard of is still an observation worth keeping, and refusing to
    /// read the database over it would be a far worse outcome than filing it
    /// under the catch-all kind.
    pub fn from_storage(raw: &str) -> Self {
        match raw {
            "session-start" => Self::SessionStart,
            "user-prompt" => Self::UserPrompt,
            "tool-use" => Self::ToolUse,
            "pre-compact" => Self::PreCompact,
            "post-compact" => Self::PostCompact,
            "session-end" => Self::SessionEnd,
            _ => Self::Notification,
        }
    }

    /// Whether this event only marks a boundary and carries no content worth
    /// consolidating. A session made only of these is closed without a page.
    pub fn is_boundary_only(&self) -> bool {
        matches!(self, Self::SessionStart | Self::SessionEnd)
    }

    /// Byte budget that applies to bodies of this kind.
    pub fn body_limit(&self) -> usize {
        match self {
            Self::Notification => BoundedBody::NOTIFICATION_LIMIT,
            _ => BoundedBody::DEFAULT_LIMIT,
        }
    }
}

/// A tool invocation referenced by an observation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolRef {
    /// Tool name as reported by the harness.
    pub name: String,
    /// Whether the call succeeded, when the harness reports it.
    pub ok: Option<bool>,
}

/// Text held to a byte budget.
///
/// The bound is part of the type rather than a convention at the call site:
/// every path that records an observation goes through this constructor, so no
/// caller can forget to apply it and let a multi-megabyte tool result into the
/// spool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoundedBody {
    text: String,
    truncated: bool,
}

impl BoundedBody {
    /// Budget for ordinary bodies.
    pub const DEFAULT_LIMIT: usize = 16 * 1024;

    /// Budget for harness notifications.
    pub const NOTIFICATION_LIMIT: usize = 2 * 1024;

    /// Hold `text` to `limit` bytes, cutting it short if necessary.
    ///
    /// Truncation lands on a UTF-8 boundary, so the result is always valid text
    /// even when the cut falls inside a multi-byte character.
    pub fn truncating(text: impl Into<String>, limit: usize) -> Self {
        let mut text = text.into();
        if text.len() <= limit {
            return Self {
                text,
                truncated: false,
            };
        }
        let mut end = limit;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        Self {
            text,
            truncated: true,
        }
    }

    /// Reject `text` outright when it exceeds `limit`.
    pub fn strict(text: impl Into<String>, limit: usize) -> Result<Self> {
        let text = text.into();
        if text.len() > limit {
            return Err(CoreError::BodyTooLarge {
                actual: text.len(),
                limit,
            });
        }
        Ok(Self {
            text,
            truncated: false,
        })
    }

    /// Rebuild a body from storage, where the bound was already applied.
    ///
    /// Only for reading back what was written. New content must go through
    /// [`BoundedBody::truncating`] or [`BoundedBody::strict`] so the budget is
    /// enforced exactly once, at capture time.
    pub fn from_stored(text: impl Into<String>, truncated: bool) -> Self {
        Self {
            text: text.into(),
            truncated,
        }
    }

    /// Borrow the retained text.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether content was dropped to fit the budget.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Retained size in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether anything was retained at all.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// A single captured lifecycle event.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Observation {
    /// Minted, time-ordered identifier.
    pub id: ObservationId,
    /// Session this belongs to.
    pub session_id: SessionId,
    /// Which boundary it was captured at.
    pub kind: EventKind,
    /// Tool involved, for [`EventKind::ToolUse`].
    pub tool: Option<ToolRef>,
    /// When it happened.
    pub at: Timestamp,
    /// Bounded payload.
    pub body: BoundedBody,
    /// Whether redaction has already been applied.
    pub sanitized: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_bodies_pass_through_untouched() {
        let body = BoundedBody::truncating("hello", BoundedBody::DEFAULT_LIMIT);
        assert_eq!(body.as_str(), "hello");
        assert!(!body.is_truncated());
    }

    #[test]
    fn truncation_lands_on_a_char_boundary() {
        // Four-byte characters, cut at a limit that falls mid-character.
        let text = "🙂".repeat(10);
        let body = BoundedBody::truncating(text, 10);
        assert!(body.is_truncated());
        assert_eq!(body.len(), 8);
        assert_eq!(body.as_str(), "🙂🙂");
    }

    #[test]
    fn strict_mode_reports_the_overflow() {
        let err = BoundedBody::strict("abcdef", 3).unwrap_err();
        assert!(matches!(
            err,
            CoreError::BodyTooLarge {
                actual: 6,
                limit: 3
            }
        ));
    }

    #[test]
    fn notifications_get_a_smaller_budget() {
        assert_eq!(
            EventKind::Notification.body_limit(),
            BoundedBody::NOTIFICATION_LIMIT
        );
        assert_eq!(EventKind::ToolUse.body_limit(), BoundedBody::DEFAULT_LIMIT);
    }

    #[test]
    fn boundary_only_events_are_identified() {
        assert!(EventKind::SessionStart.is_boundary_only());
        assert!(EventKind::SessionEnd.is_boundary_only());
        assert!(!EventKind::UserPrompt.is_boundary_only());
    }

    #[test]
    fn event_kind_round_trips_through_serde() {
        let json = serde_json::to_string(&EventKind::PreCompact).unwrap();
        assert_eq!(json, "\"pre-compact\"");
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EventKind::PreCompact);
    }

    #[test]
    fn event_kind_round_trips_through_storage() {
        for kind in [
            EventKind::SessionStart,
            EventKind::UserPrompt,
            EventKind::ToolUse,
            EventKind::PreCompact,
            EventKind::PostCompact,
            EventKind::SessionEnd,
            EventKind::Notification,
        ] {
            assert_eq!(EventKind::from_storage(kind.as_str()), kind);
        }
        // An event a newer build wrote is filed under the catch-all rather
        // than costing the reader the whole row.
        assert_eq!(
            EventKind::from_storage("some-future-event"),
            EventKind::Notification
        );
    }
}
