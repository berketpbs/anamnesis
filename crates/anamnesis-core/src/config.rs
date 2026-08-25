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

use crate::decay::DecayParams;
use crate::error::{CoreError, Result};
use crate::scope::{ProjectName, WorkspaceName};
use crate::sweep::SweepPolicy;

/// Contents of a `.anamnesis.toml` marker file.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarkerConfig {
    /// Explicit workspace and project overrides.
    pub scope: ScopeConfig,
    /// Which events never reach the spool.
    pub capture: CaptureConfig,
    /// How quickly unused pages are forgotten.
    pub decay: DecayConfig,
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

/// Retention tuning for the decay sweep.
///
/// Expressed in half-lives rather than the exponential rates the formula
/// actually uses: "unread pages lose half their weight every 30 days" is a
/// sentence someone can hold an opinion about, and `lambda = 0.0231` is not.
/// The conversion happens in [`Self::policy`], which is also where a value
/// that would make the sweep nonsense is refused.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecayConfig {
    /// Retention score below which an unexempt page is forgotten.
    pub threshold: f64,
    /// Days for the age term to halve.
    pub age_half_life_days: f64,
    /// Days for the access term to halve once a page stops being read.
    pub access_half_life_days: f64,
    /// Weight of the access term relative to the age term.
    pub access_weight: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        let params = DecayParams::default();
        Self {
            threshold: SweepPolicy::DEFAULT_THRESHOLD,
            age_half_life_days: DecayParams::AGE_HALF_LIFE_DAYS,
            access_half_life_days: DecayParams::ACCESS_HALF_LIFE_DAYS,
            access_weight: params.sigma,
        }
    }
}

impl DecayConfig {
    /// Turn the configured half-lives into the policy a sweep runs with.
    ///
    /// Rejects rather than clamps. A half-life of zero divides by zero and
    /// makes every page immortal; a negative one makes pages *gain* weight
    /// with age. Both are typos, and the failure mode of guessing is a sweep
    /// that deletes the wrong pages while reporting nothing unusual.
    pub fn policy(&self) -> Result<SweepPolicy> {
        Self::positive("decay.age_half_life_days", self.age_half_life_days)?;
        Self::positive("decay.access_half_life_days", self.access_half_life_days)?;
        Self::not_negative("decay.access_weight", self.access_weight)?;
        Self::not_negative("decay.threshold", self.threshold)?;

        Ok(SweepPolicy {
            params: DecayParams {
                lambda: std::f64::consts::LN_2 / self.age_half_life_days,
                sigma: self.access_weight,
                mu: std::f64::consts::LN_2 / self.access_half_life_days,
            },
            threshold: self.threshold,
        })
    }

    /// Reject a value that is not a finite number greater than zero.
    fn positive(key: &'static str, value: f64) -> Result<()> {
        if value.is_finite() && value > 0.0 {
            return Ok(());
        }
        Err(CoreError::InvalidSetting {
            key,
            value,
            reason: "must be a finite number greater than zero",
        })
    }

    /// Reject a value that is not a finite number of zero or more.
    fn not_negative(key: &'static str, value: f64) -> Result<()> {
        if value.is_finite() && value >= 0.0 {
            return Ok(());
        }
        Err(CoreError::InvalidSetting {
            key,
            value,
            reason: "must be a finite number of zero or more",
        })
    }
}

/// Per-operator memory slot settings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlotsConfig {
    /// Give each authenticated operator an isolated slot.
    pub per_user: bool,
}

/// Automatic learning proposal settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    fn decay_defaults_match_the_formula_defaults() {
        let config = MarkerConfig::from_toml("").expect("parses");
        let policy = config.decay.policy().expect("valid");
        assert_eq!(policy, SweepPolicy::default());
    }

    #[test]
    fn half_lives_become_rates() {
        let config = MarkerConfig::from_toml(
            "[decay]
age_half_life_days = 10.0
access_half_life_days = 5.0
",
        )
        .expect("parses");
        let policy = config.decay.policy().expect("valid");

        // A rate is only meaningful through what it does to a score: after one
        // half-life, half remains.
        let inputs = crate::decay::DecayInputs::unread(1.0, 10.0);
        let halved = crate::decay::retention_score(inputs, policy.params);
        assert!((halved - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_zero_half_life_is_refused_rather_than_dividing_by_zero() {
        let config = MarkerConfig::from_toml(
            "[decay]
age_half_life_days = 0.0
",
        )
        .expect("parses");
        let error = config.decay.policy().expect_err("rejected");
        assert!(error.to_string().contains("decay.age_half_life_days"));
    }

    #[test]
    fn a_negative_threshold_is_refused() {
        let config = MarkerConfig::from_toml(
            "[decay]
threshold = -1.0
",
        )
        .expect("parses");
        assert!(config.decay.policy().is_err());
    }

    #[test]
    fn a_misspelled_decay_key_is_reported() {
        // The sweep deletes pages. A tuning key that silently does nothing is
        // the difference between "forgets after a year" and "forgets today".
        assert!(
            MarkerConfig::from_toml(
                "[decay]
threshhold = 0.1
"
            )
            .is_err()
        );
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

[decay]
threshold = 0.05
age_half_life_days = 30.0
access_half_life_days = 14.0
access_weight = 0.5
"#,
        )
        .expect("parses");

        assert_eq!(
            config.scope.project.expect("project set").as_str(),
            "anamnesis"
        );
        assert_eq!(config.capture.ignore_paths.len(), 2);
        assert_eq!(
            config.decay.policy().expect("valid"),
            SweepPolicy::default()
        );
    }
}
