//! Agent sessions.

use jiff::Timestamp;

use crate::ids::{ProjectId, SessionId, WorkspaceId, WorkstreamId};

/// Which harness produced a session.
///
/// [`AgentKind::Other`] exists deliberately. The upstream project encoded its
/// agent list in a SQL `CHECK` constraint and needed eleven separate schema
/// migrations to add harnesses one at a time; keeping the set open here means
/// supporting a new agent is a parsing change, not a migration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// Cursor.
    Cursor,
    /// Gemini CLI.
    GeminiCli,
    /// OpenCode.
    OpenCode,
    /// Any harness not known at compile time.
    Other(String),
}

impl AgentKind {
    /// Canonical lowercase identifier, as stored in the database.
    pub fn as_str(&self) -> &str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::GeminiCli => "gemini-cli",
            Self::OpenCode => "opencode",
            Self::Other(name) => name,
        }
    }

    /// Whether this harness is recognised at compile time.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl std::str::FromStr for AgentKind {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
        Ok(match normalized.as_str() {
            "claude-code" | "claude" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "cursor" => Self::Cursor,
            "gemini-cli" | "gemini" => Self::GeminiCli,
            "opencode" => Self::OpenCode,
            _ => Self::Other(normalized),
        })
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for AgentKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for AgentKind {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Lifecycle state of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    /// Receiving observations.
    Open,
    /// Ended and awaiting consolidation.
    Ending,
    /// Consolidated; a page and handoff may exist.
    Closed,
}

/// One bounded unit of agent work.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    /// Minted, time-ordered identifier.
    pub id: SessionId,
    /// Harness that produced it.
    pub agent: AgentKind,
    /// Workspace it belongs to.
    pub workspace_id: WorkspaceId,
    /// Project it belongs to.
    pub project_id: ProjectId,
    /// The workstream this session is part of, if any. `None` is the
    /// ordinary case — a project with one thread of work needs no workstream
    /// at all.
    pub workstream_id: Option<WorkstreamId>,
    /// Working directory the agent ran in.
    pub checkout_path: std::path::PathBuf,
    /// When the session started.
    pub started_at: Timestamp,
    /// When the session ended, if it has.
    pub ended_at: Option<Timestamp>,
    /// Current lifecycle state.
    pub state: SessionState,
}

impl Session {
    /// Whether the session is still accepting observations.
    pub fn is_open(&self) -> bool {
        matches!(self.state, SessionState::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_agents_parse_to_variants() {
        assert_eq!("claude-code".parse::<AgentKind>().unwrap(), AgentKind::ClaudeCode);
        assert_eq!("Claude_Code".parse::<AgentKind>().unwrap(), AgentKind::ClaudeCode);
        assert_eq!("  CODEX ".parse::<AgentKind>().unwrap(), AgentKind::Codex);
    }

    #[test]
    fn unknown_agents_are_preserved_not_rejected() {
        let agent: AgentKind = "Kiro_CLI".parse().unwrap();
        assert_eq!(agent, AgentKind::Other("kiro-cli".to_owned()));
        assert_eq!(agent.as_str(), "kiro-cli");
        assert!(!agent.is_known());
    }

    #[test]
    fn agent_round_trips_through_serde() {
        for agent in [AgentKind::ClaudeCode, AgentKind::Other("devin".to_owned())] {
            let json = serde_json::to_string(&agent).unwrap();
            let back: AgentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(agent, back);
        }
    }
}
