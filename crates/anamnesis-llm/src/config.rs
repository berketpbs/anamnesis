//! Where the model settings come from, and what happens when they are absent.
//!
//! Absence is the normal case. Anamnesis is useful with no key configured at
//! all, so nothing here fails merely because a variable is unset — it returns
//! "no provider" and the deterministic path takes over. Configuration only
//! *errors* when someone has clearly tried and got it wrong: a provider named
//! that does not exist, an effort level that is not one of the five, a
//! provider selected with no key to use. Those are typos worth surfacing;
//! silence would send every session to the fallback with no explanation.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

use crate::LlmError;
use crate::anthropic::Anthropic;
use crate::provider::Provider;

/// Which backend to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// The Anthropic Messages API.
    #[default]
    Anthropic,
    /// No model. Consolidation stays deterministic.
    None,
}

impl FromStr for ProviderKind {
    type Err = LlmError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "none" | "off" | "disabled" => Ok(Self::None),
            other => Err(LlmError::Config(format!(
                "unknown provider {other:?}; expected \"anthropic\" or \"none\""
            ))),
        }
    }
}

/// How hard the model should think before answering.
///
/// Consolidation is not a reasoning problem, but it is a *judgement* problem —
/// deciding which of forty tool calls mattered is exactly the part the
/// deterministic path cannot do. The default matches the API's own default
/// rather than trying to be clever about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effort {
    /// Cheapest.
    Low,
    /// Middle.
    Medium,
    /// The API default.
    #[default]
    High,
    /// Above high.
    XHigh,
    /// Correctness over cost.
    Max,
}

impl Effort {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl FromStr for Effort {
    type Err = LlmError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(LlmError::Config(format!(
                "unknown effort {other:?}; expected low, medium, high, xhigh, or max"
            ))),
        }
    }
}

/// Default model.
///
/// Consolidation is the one place anamnesis spends money on someone's behalf,
/// and the thing being produced is the memory every later session reads. A
/// cheaper default would quietly make every future session slightly worse for
/// a saving nobody asked for, so the choice is left to whoever is paying:
/// `ANAMNESIS_LLM_MODEL` overrides this.
const DEFAULT_MODEL: &str = "claude-opus-5";

/// Default API root.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Default ceiling on prompt size, in estimated tokens.
///
/// Small enough that a locally hosted model with a modest window can be
/// pointed at the same code path, which is the reason to have a budget at all
/// rather than sending whatever a session happened to produce.
const DEFAULT_MAX_INPUT_TOKENS: usize = 6_500;

/// Default ceiling on the reply.
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 2_000;

/// Floor on the reply ceiling. A page and a handoff do not fit in less.
const MIN_MAX_OUTPUT_TOKENS: u32 = 1_000;

/// Default per-request timeout.
///
/// Generous, because this runs after the session has already ended and no
/// human is waiting on it — unlike the capture path, where a slow response is
/// felt immediately.
const DEFAULT_TIMEOUT_SECS: u64 = 90;

/// Everything needed to build a provider.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Which backend.
    pub provider: ProviderKind,
    /// Credential, when the backend needs one.
    pub api_key: Option<SecretString>,
    /// Model identifier sent on every request.
    pub model: String,
    /// API root, overridable for gateways and local servers.
    pub base_url: String,
    /// Prompt budget, in estimated tokens.
    pub max_input_tokens: usize,
    /// Reply ceiling, in tokens.
    pub max_output_tokens: u32,
    /// Thinking depth.
    pub effort: Effort,
    /// Per-request timeout.
    pub timeout: Duration,
    /// How many times a retryable failure is retried.
    pub max_retries: u32,
    /// Whether to let the API re-run a declined request on another model.
    pub server_side_fallbacks: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::None,
            api_key: None,
            model: DEFAULT_MODEL.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            max_input_tokens: DEFAULT_MAX_INPUT_TOKENS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            effort: Effort::default(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            max_retries: 2,
            server_side_fallbacks: true,
        }
    }
}

impl LlmConfig {
    /// Read settings from the process environment.
    pub fn from_env() -> Result<Self, LlmError> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// Read settings from an arbitrary lookup, so this is testable without
    /// mutating the environment of a parallel test run.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, LlmError> {
        let mut config = Self::default();

        let key = var("ANAMNESIS_LLM_API_KEY")
            .or_else(|| var("ANTHROPIC_API_KEY"))
            .filter(|value| !value.trim().is_empty());

        // Selection is implicit by default: a key present means someone wants
        // a model used. Being explicit is still possible, and is the only way
        // to turn a model *off* without unsetting a key other tools may want.
        config.provider = match var("ANAMNESIS_LLM_PROVIDER") {
            Some(value) => value.parse()?,
            None if key.is_some() => ProviderKind::Anthropic,
            None => ProviderKind::None,
        };
        config.api_key = key.map(SecretString::from);

        if config.provider == ProviderKind::Anthropic && config.api_key.is_none() {
            return Err(LlmError::Config(
                "provider is anthropic but no ANTHROPIC_API_KEY (or ANAMNESIS_LLM_API_KEY) is set"
                    .to_owned(),
            ));
        }

        if let Some(model) = var("ANAMNESIS_LLM_MODEL").filter(|v| !v.trim().is_empty()) {
            config.model = model.trim().to_owned();
        }
        if let Some(url) = var("ANAMNESIS_LLM_BASE_URL").filter(|v| !v.trim().is_empty()) {
            config.base_url = url.trim().trim_end_matches('/').to_owned();
        }
        if let Some(effort) = var("ANAMNESIS_LLM_EFFORT") {
            config.effort = effort.parse()?;
        }
        if let Some(value) = var("ANAMNESIS_LLM_MAX_INPUT_TOKENS") {
            config.max_input_tokens = parse_number(&value, "ANAMNESIS_LLM_MAX_INPUT_TOKENS")?;
        }
        if let Some(value) = var("ANAMNESIS_LLM_MAX_OUTPUT_TOKENS") {
            let requested: u32 = parse_number(&value, "ANAMNESIS_LLM_MAX_OUTPUT_TOKENS")?;
            // Clamped rather than rejected: someone economising should get a
            // short summary, not a session that silently loses its page.
            config.max_output_tokens = requested.max(MIN_MAX_OUTPUT_TOKENS);
        }
        if let Some(value) = var("ANAMNESIS_LLM_TIMEOUT_SECS") {
            config.timeout =
                Duration::from_secs(parse_number(&value, "ANAMNESIS_LLM_TIMEOUT_SECS")?);
        }
        if let Some(value) = var("ANAMNESIS_LLM_MAX_RETRIES") {
            config.max_retries = parse_number(&value, "ANAMNESIS_LLM_MAX_RETRIES")?;
        }
        if let Some(value) = var("ANAMNESIS_LLM_FALLBACKS") {
            config.server_side_fallbacks = !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            );
        }

        Ok(config)
    }

    /// Build the provider this configuration describes.
    ///
    /// `None` is a successful outcome, not a failure: it is what "run without
    /// a model" looks like.
    pub fn build(&self) -> Result<Option<Arc<dyn Provider>>, LlmError> {
        match self.provider {
            ProviderKind::None => Ok(None),
            ProviderKind::Anthropic => Ok(Some(Arc::new(Anthropic::new(self)?))),
        }
    }
}

/// Read the environment and build a provider, or none.
pub fn provider_from_env() -> Result<Option<Arc<dyn Provider>>, LlmError> {
    LlmConfig::from_env()?.build()
}

/// Parse a numeric setting, naming the variable when it does not parse.
fn parse_number<T: FromStr>(value: &str, name: &str) -> Result<T, LlmError> {
    value
        .trim()
        .parse()
        .map_err(|_| LlmError::Config(format!("{name} is not a number: {value:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// A lookup over a fixed table, so no test has to touch the real
    /// environment — which is process-global and shared with every other test
    /// running at the same time.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_owned())
        }
    }

    #[test]
    fn an_empty_environment_means_no_model_not_an_error() {
        let config = LlmConfig::from_vars(vars(&[])).expect("no error");
        assert_eq!(config.provider, ProviderKind::None);
        assert!(config.build().expect("builds").is_none());
    }

    #[test]
    fn a_key_alone_selects_anthropic() {
        let config =
            LlmConfig::from_vars(vars(&[("ANTHROPIC_API_KEY", "sk-ant-test")])).expect("no error");
        assert_eq!(config.provider, ProviderKind::Anthropic);
        assert_eq!(config.model, DEFAULT_MODEL);
        assert_eq!(
            config.api_key.expect("key kept").expose_secret(),
            "sk-ant-test"
        );
    }

    #[test]
    fn a_blank_key_is_the_same_as_no_key() {
        // An exported-but-empty variable is a very common shell accident, and
        // treating it as "configured" would turn every session end into a 401.
        let config = LlmConfig::from_vars(vars(&[("ANTHROPIC_API_KEY", "   ")])).expect("no error");
        assert_eq!(config.provider, ProviderKind::None);
    }

    #[test]
    fn a_model_can_be_turned_off_without_unsetting_the_key() {
        let config = LlmConfig::from_vars(vars(&[
            ("ANTHROPIC_API_KEY", "sk-ant-test"),
            ("ANAMNESIS_LLM_PROVIDER", "none"),
        ]))
        .expect("no error");
        assert_eq!(config.provider, ProviderKind::None);
        assert!(config.build().expect("builds").is_none());
    }

    #[test]
    fn asking_for_anthropic_without_a_key_is_reported() {
        let error = LlmConfig::from_vars(vars(&[("ANAMNESIS_LLM_PROVIDER", "anthropic")]))
            .expect_err("no key");
        assert!(matches!(error, LlmError::Config(_)));
    }

    #[test]
    fn a_misspelled_provider_is_reported_rather_than_ignored() {
        let error = LlmConfig::from_vars(vars(&[("ANAMNESIS_LLM_PROVIDER", "anthropc")]))
            .expect_err("typo");
        assert!(error.to_string().contains("anthropc"));
    }

    #[test]
    fn output_budget_is_clamped_up_to_something_a_page_fits_in() {
        let config = LlmConfig::from_vars(vars(&[
            ("ANTHROPIC_API_KEY", "k"),
            ("ANAMNESIS_LLM_MAX_OUTPUT_TOKENS", "10"),
        ]))
        .expect("no error");
        assert_eq!(config.max_output_tokens, MIN_MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn a_non_numeric_budget_names_the_variable_it_came_from() {
        let error = LlmConfig::from_vars(vars(&[
            ("ANTHROPIC_API_KEY", "k"),
            ("ANAMNESIS_LLM_MAX_INPUT_TOKENS", "lots"),
        ]))
        .expect_err("not a number");
        assert!(error.to_string().contains("ANAMNESIS_LLM_MAX_INPUT_TOKENS"));
    }

    #[test]
    fn a_base_url_loses_its_trailing_slash_so_paths_join_cleanly() {
        let config = LlmConfig::from_vars(vars(&[
            ("ANTHROPIC_API_KEY", "k"),
            ("ANAMNESIS_LLM_BASE_URL", "http://localhost:4000/"),
        ]))
        .expect("no error");
        assert_eq!(config.base_url, "http://localhost:4000");
    }

    #[test]
    fn effort_is_validated_at_load_time() {
        assert!(
            LlmConfig::from_vars(vars(&[
                ("ANTHROPIC_API_KEY", "k"),
                ("ANAMNESIS_LLM_EFFORT", "maximum"),
            ]))
            .is_err()
        );
        let config = LlmConfig::from_vars(vars(&[
            ("ANTHROPIC_API_KEY", "k"),
            ("ANAMNESIS_LLM_EFFORT", "XHigh"),
        ]))
        .expect("no error");
        assert_eq!(config.effort, Effort::XHigh);
    }

    #[test]
    fn fallbacks_are_on_unless_explicitly_refused() {
        let on = LlmConfig::from_vars(vars(&[("ANTHROPIC_API_KEY", "k")])).expect("no error");
        assert!(on.server_side_fallbacks);
        let off = LlmConfig::from_vars(vars(&[
            ("ANTHROPIC_API_KEY", "k"),
            ("ANAMNESIS_LLM_FALLBACKS", "off"),
        ]))
        .expect("no error");
        assert!(!off.server_side_fallbacks);
    }
}
