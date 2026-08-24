//! Workstreams: named, persistent threads of work that can span many
//! sessions and many harnesses.
//!
//! A session ending with a handoff is already enough to resume *the* most
//! recent thread of work in a project — that is what [`crate::handoff::Handoff`]
//! does on its own, one pending note per project. A workstream exists for the
//! case where a project has more than one thread running at once
//! ("auth-refactor" and "bug-123" in flight together): each gets its own
//! slug, its own status, and — because a handoff's uniqueness is keyed on
//! `(project, workstream)` rather than `project` alone — its own pending
//! handoff slot, so resuming one does not consume or shadow the other.
//! Nothing about a project with a single thread of work changes: a session
//! that never names a workstream behaves exactly as it did before this
//! module existed.

use jiff::Timestamp;

use crate::error::{CoreError, Result};
use crate::ids::{ProjectId, WorkstreamId};

/// Longest permitted slug, in bytes.
pub const MAX_SLUG_LEN: usize = 64;

/// A validated, project-relative workstream slug such as `auth-refactor`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct WorkstreamSlug(String);

impl WorkstreamSlug {
    /// Validate a slug supplied by a caller.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let invalid = |reason: &'static str| CoreError::InvalidName {
            kind: "workstream slug",
            value: value.to_owned(),
            reason,
        };

        if trimmed.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if trimmed.len() > MAX_SLUG_LEN {
            return Err(invalid("longer than 64 bytes"));
        }
        if trimmed.chars().any(|c| c.is_control()) {
            return Err(invalid("must not contain control characters"));
        }
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(invalid(
                "may only contain ASCII letters, digits, '-', and '_'",
            ));
        }

        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    /// Borrow the slug as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkstreamSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for WorkstreamSlug {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Lifecycle state of a workstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkstreamStatus {
    /// Open to new sessions and new pending handoffs.
    #[default]
    Active,
    /// Not being worked on, but not finished either. Sessions can still join
    /// it explicitly; it just is not implied to be the thread someone means
    /// by default.
    Paused,
    /// Done. Kept for its event ledger, not for further work.
    Completed,
}

impl WorkstreamStatus {
    /// Canonical lowercase identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
        }
    }
}

/// A named, persistent thread of work.
#[derive(Debug, Clone)]
pub struct Workstream {
    /// Derived identifier.
    pub id: WorkstreamId,
    /// Project the workstream belongs to.
    pub project_id: ProjectId,
    /// Stable short name, used to resume it and to key its handoff slot.
    pub slug: WorkstreamSlug,
    /// Human-facing title.
    pub title: String,
    /// Current lifecycle state.
    pub status: WorkstreamStatus,
    /// When it was first started.
    pub created_at: Timestamp,
    /// When it was last touched (status change, or a session joining it).
    pub updated_at: Timestamp,
}

impl Workstream {
    /// Start (or describe) a workstream, deriving its identifier from
    /// `project` and `slug`.
    pub fn new(project_id: ProjectId, slug: WorkstreamSlug, title: impl Into<String>, now: Timestamp) -> Self {
        Self {
            id: WorkstreamId::derive(project_id, slug.as_str()),
            project_id,
            slug,
            title: title.into(),
            status: WorkstreamStatus::default(),
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercased_and_trimmed() {
        assert_eq!(WorkstreamSlug::parse("  Auth-Refactor  ").unwrap().as_str(), "auth-refactor");
    }

    #[test]
    fn slugs_reject_slashes_and_spaces() {
        for bad in ["a/b", "a b", "", "   ", &"x".repeat(65)] {
            assert!(WorkstreamSlug::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn a_workstreams_identifier_is_derived_from_its_slug() {
        let project = ProjectId::from_uuid(uuid::Uuid::nil());
        let slug = WorkstreamSlug::parse("auth-refactor").unwrap();
        let workstream = Workstream::new(project, slug, "Auth refactor", Timestamp::now());
        assert_eq!(
            workstream.id,
            crate::ids::WorkstreamId::derive(project, "auth-refactor")
        );
    }
}
