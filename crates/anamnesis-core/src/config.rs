//! Configuration read from the per-project marker file.
//!
//! The marker describes one project and is deliberately file-only: no
//! environment layer is merged in here. A stray `ANAMNESIS_*` variable meant
//! for the server must not be able to re-scope a repository, and unknown keys
//! are rejected rather than ignored so a typo surfaces instead of silently
//! sending memory to the wrong project.

use std::path::Path;

use figment::Figment;
use figment::providers::{Format, Toml};

use crate::error::Result;
use crate::scope::{ProjectName, WorkspaceName};

/// Contents of a `.anamnesis.toml` marker file.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarkerConfig {
    /// Explicit workspace and project overrides.
    pub scope: ScopeConfig,
    /// Which events never reach the spool.
    pub capture: CaptureConfig,
    /// Per-operator memory slots.
    pub slots: SlotsConfig,
    /// Automatic learning proposals.
    pub auto_improve: AutoImproveConfig,
}

impl MarkerConfig {
    /// Load a marker file from disk.
    pub fn load(path: &Path) -> Result<Self> {
        Ok(Figment::new().merge(Toml::file(path)).extract()?)
    }

    /// Parse a marker file from a string, for tests and dry runs.
    pub fn from_toml(source: &str) -> Result<Self> {
        Ok(Figment::new().merge(Toml::string(source)).extract()?)
    }
}

/// Explicit scope overrides. Either field may be omitted, in which case the
/// value is inferred from the repository.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeConfig {
    /// Workspace this project belongs to.
    pub workspace: Option<WorkspaceName>,
    /// Pinned project name. Overrides git-derived identity.
    pub project: Option<ProjectName>,
}

/// Capture exclusions applied before events reach the spool.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Glob patterns whose file events are dropped entirely.
    pub ignore_paths: Vec<String>,
}

/// Per-operator memory slot settings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlotsConfig {
    /// Give each authenticated operator an isolated slot.
    pub per_user: bool,
}

/// Automatic learning proposal settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoImproveConfig {
    /// Whether proposals are generated at all.
    pub enabled: bool,
    /// Hold proposals for human approval before they are applied.
    pub require_approval: bool,
    /// Background scheduling. A single table, never a list.
    pub scheduler: SchedulerConfig,
}

impl Default for AutoImproveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_approval: true,
            scheduler: SchedulerConfig::default(),
        }
    }
}

/// Background scheduler settings for auto-improvement.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Whether the scheduler runs.
    pub enabled: bool,
    /// Minutes between runs.
    pub interval_minutes: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_marker_yields_defaults() {
        let config = MarkerConfig::from_toml("").expect("parses");
        assert!(config.scope.workspace.is_none());
        assert!(config.scope.project.is_none());
        assert!(config.auto_improve.require_approval);
        assert!(!config.slots.per_user);
    }

    #[test]
    fn scheduler_is_a_single_table() {
        let config = MarkerConfig::from_toml(
            "[auto_improve.scheduler]\nenabled = true\ninterval_minutes = 30\n",
        )
        .expect("parses");
        assert!(config.auto_improve.scheduler.enabled);
        assert_eq!(config.auto_improve.scheduler.interval_minutes, 30);
    }

    #[test]
    fn scheduler_written_as_an_array_is_rejected() {
        // `[[auto_improve.scheduler]]` is a table *array*; accepting it silently
        // would mean the schedule is quietly ignored at runtime.
        let err = MarkerConfig::from_toml(
            "[[auto_improve.scheduler]]\nenabled = true\ninterval_minutes = 30\n",
        );
        assert!(err.is_err());
    }

    #[test]
    fn unknown_keys_are_reported_rather_than_ignored() {
        assert!(MarkerConfig::from_toml("[scope]\nworkspce = \"typo\"\n").is_err());
    }

    #[test]
    fn invalid_names_fail_at_load_time() {
        assert!(MarkerConfig::from_toml("[scope]\nproject = \"../escape\"\n").is_err());
    }

    #[test]
    fn full_marker_round_trips() {
        let config = MarkerConfig::from_toml(
            r#"
[scope]
workspace = "default"
project = "anamnesis"

[capture]
ignore_paths = ["target/**", "*.lock"]

[slots]
per_user = false

[auto_improve]
enabled = true
require_approval = true

[auto_improve.scheduler]
enabled = false
interval_minutes = 60
"#,
        )
        .expect("parses");

        assert_eq!(
            config.scope.project.expect("project set").as_str(),
            "anamnesis"
        );
        assert_eq!(config.capture.ignore_paths.len(), 2);
    }
}
