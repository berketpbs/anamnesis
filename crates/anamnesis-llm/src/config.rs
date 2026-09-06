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
use crate::openai::OpenAiCompatible;
use crate::provider::Provider;

/// Which backend to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// The Anthropic Messages API.
    #[default]
    Anthropic,
    /// The OpenAI chat-completions API, or a gateway presenting it.
    OpenAi,
    /// Ollama, on this machine. The same wire format as [`ProviderKind::OpenAi`]
    /// with a different default address and no credential to present — kept
    /// separate so that "consolidation ran locally" is a thing the logs can
    /// say.
    Ollama,
    /// Google AI Studio, through the OpenAI-compatible surface Gemini
    /// publishes. Again the same wire format, again separate for the same two
    /// reasons: its own address and model, and a name the logs can print.
    Google,
    /// No model. Consolidation stays deterministic.
    None,
}

impl FromStr for ProviderKind {
    type Err = LlmError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "openai" | "openai-compatible" | "oai" => Ok(Self::OpenAi),
            "ollama" | "local" => Ok(Self::Ollama),
            "google" | "gemini" | "aistudio" | "google-ai-studio" => Ok(Self::Google),
            "none" | "off" | "disabled" => Ok(Self::None),
            other => Err(LlmError::Config(format!(
                "unknown provider {other:?}; expected \"anthropic\", \"openai\", \"google\", \"ollama\" or \"none\""
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
///
/// Ordered from cheapest to most thorough, and the order is load-bearing:
/// a backend whose vocabulary stops short of the top can say so with a
/// ceiling rather than with a special case per level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
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

/// Default API root for Anthropic.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Default API root for the OpenAI chat-completions API.
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Default API root for Ollama, which listens here unless told otherwise.
const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434/v1";

/// Default model for Ollama.
///
/// Named rather than left as the Anthropic default, which no local server has
/// ever heard of: a first run that has to be told the model as well as the
/// provider is a first run most people abandon. Wrong for anyone who pulled
/// something else, and `ANAMNESIS_LLM_MODEL` is one variable.
const DEFAULT_OLLAMA_MODEL: &str = "llama3.2";

/// Default API root for Google AI Studio.
///
/// The OpenAI-compatible surface rather than Gemini's own `generateContent`,
/// because the compatible one is the same wire format every other backend here
/// already speaks: one non-streaming POST and a reply constrained by a JSON
/// schema. Writing a second client for a second shape of the same request is
/// how the two drift.
const DEFAULT_GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

/// Default model for Google AI Studio.
///
/// Named for the same reason Ollama's is: a first run that has to be told the
/// model as well as the provider is a first run most people abandon.
///
/// This one was picked by asking rather than by reading a table, and the
/// asking is the point: `gemini-2.5-flash` is **listed** by the models
/// endpoint and answers a completion with `404 ... no longer available to new
/// users`. A listing is not a list of what a key may call. The name here is
/// the one Google's own retirement notice sends people to, so it is the one a
/// new key is most likely to reach; newer flashes exist and are one variable
/// away.
const DEFAULT_GOOGLE_MODEL: &str = "gemini-3.6-flash";

/// The most effort Google's compatible surface has a word for.
///
/// It takes `minimal`, `low`, `medium`, `high` and `none`; `xhigh` and `max`
/// are ours and mean nothing there. The refusal is a plain 400 that says
/// nothing about thinking, so the fallback in `openai.rs` — which drops the
/// field when a backend names thinking — cannot catch it, and a session would
/// lose its page over a setting the model never needed. Clamped here instead,
/// where the answer is known rather than guessed from an error message.
const GOOGLE_MAX_EFFORT: Effort = Effort::High;

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

/// Retries for a caller that is waiting: a person at a terminal, or a request
/// holding a connection open. Two spends itself in about three seconds.
const DEFAULT_MAX_RETRIES: u32 = 2;

/// Retries for work nobody is waiting on.
///
/// The server's consolidation is spawned and detached — no request, no person,
/// nothing that times out — so the only cost of trying again is a page
/// arriving later, and the cost of giving up is a session summarised as counts
/// forever, since `reconsolidate` can rewrite the page but deliberately leaves
/// no handoff.
///
/// The failure this is sized for happened on 2026-09-05. The reap pass runs
/// when the server starts, and it asked the model 28 ms after the process
/// began, before this machine's network was up. Three attempts inside three
/// seconds all returned a transport error, the fallback wrote counts, and the
/// largest session in the index lost both its summary and its handoff to a
/// network that answered a moment later.
///
/// Against the backoff in `http::retry_delay` — 2, 4, 8, 16 seconds, then 30
/// apiece — eight retries is about two and a half minutes.
const BACKGROUND_MAX_RETRIES: u32 = 8;

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
            max_retries: DEFAULT_MAX_RETRIES,
            server_side_fallbacks: true,
        }
    }
}

impl LlmConfig {
    /// Read settings from the process environment.
    pub fn from_env() -> Result<Self, LlmError> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// Read settings for a process whose model calls nobody waits on.
    ///
    /// Identical to [`LlmConfig::from_env`] but for where the retry budget
    /// starts: `BACKGROUND_MAX_RETRIES` rather than the budget sized for a
    /// caller holding a connection open. `ANAMNESIS_LLM_MAX_RETRIES` still
    /// wins over both, so an operator who has chosen a number keeps it.
    pub fn from_env_unhurried() -> Result<Self, LlmError> {
        Self::from_vars_starting_at(BACKGROUND_MAX_RETRIES, |key| std::env::var(key).ok())
    }

    /// Read settings from an arbitrary lookup, so this is testable without
    /// mutating the environment of a parallel test run.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, LlmError> {
        Self::from_vars_starting_at(DEFAULT_MAX_RETRIES, var)
    }

    /// `from_vars`, with the retry budget to fall back on when
    /// the environment does not name one.
    fn from_vars_starting_at(
        default_retries: u32,
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, LlmError> {
        let mut config = Self {
            max_retries: default_retries,
            ..Self::default()
        };

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

        // Defaults that depend on which backend was chosen, applied before the
        // overrides below so that an explicit `ANAMNESIS_LLM_BASE_URL` still
        // wins. Without this, choosing `ollama` would inherit Anthropic's
        // address and fail in a way that names neither.
        match config.provider {
            ProviderKind::OpenAi => config.base_url = DEFAULT_OPENAI_BASE_URL.to_owned(),
            ProviderKind::Ollama => {
                config.base_url = DEFAULT_OLLAMA_BASE_URL.to_owned();
                config.model = DEFAULT_OLLAMA_MODEL.to_owned();
            }
            ProviderKind::Google => {
                config.base_url = DEFAULT_GOOGLE_BASE_URL.to_owned();
                config.model = DEFAULT_GOOGLE_MODEL.to_owned();
            }
            ProviderKind::Anthropic | ProviderKind::None => {}
        }

        // Google's own variable names, read only once Google has been asked
        // for by name. Deliberately not part of the chain above: a Gemini key
        // left in the environment by some other tool must never become the
        // reason consolidation stopped talking to whatever it was configured
        // to talk to. A key that selects a provider is a key that can redirect
        // one.
        if config.provider == ProviderKind::Google && config.api_key.is_none() {
            config.api_key = var("GEMINI_API_KEY")
                .or_else(|| var("GOOGLE_API_KEY"))
                .filter(|value| !value.trim().is_empty())
                .map(SecretString::from);
        }

        if config.provider == ProviderKind::Anthropic && config.api_key.is_none() {
            return Err(LlmError::Config(
                "provider is anthropic but no ANTHROPIC_API_KEY (or ANAMNESIS_LLM_API_KEY) is set"
                    .to_owned(),
            ));
        }

        if config.provider == ProviderKind::Google && config.api_key.is_none() {
            return Err(LlmError::Config(
                "provider is google but no GEMINI_API_KEY (or GOOGLE_API_KEY, \
                 or ANAMNESIS_LLM_API_KEY) is set"
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
            ProviderKind::OpenAi => Ok(Some(Arc::new(OpenAiCompatible::new(self, "openai")?))),
            ProviderKind::Ollama => Ok(Some(Arc::new(OpenAiCompatible::new(self, "ollama")?))),
            ProviderKind::Google => Ok(Some(Arc::new(
                OpenAiCompatible::new(self, "google")?.with_effort_ceiling(GOOGLE_MAX_EFFORT),
            ))),
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

    /// The point of the local backend: it runs without a credential. Requiring
    /// one would make the only configuration that costs nothing, and sends
    /// nobody's session transcript anywhere, impossible to express.
    #[test]
    fn ollama_needs_no_key_and_knows_where_it_lives() {
        let config = LlmConfig::from_vars(vars(&[("ANAMNESIS_LLM_PROVIDER", "ollama")]))
            .expect("a local model needs no key");

        assert_eq!(config.provider, ProviderKind::Ollama);
        assert!(config.api_key.is_none());
        assert_eq!(config.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(config.model, "llama3.2");
        assert!(config.build().expect("builds").is_some());
    }

    #[test]
    fn openai_defaults_to_openai_rather_than_to_anthropics_address() {
        let config = LlmConfig::from_vars(vars(&[
            ("ANAMNESIS_LLM_PROVIDER", "openai"),
            ("ANAMNESIS_LLM_API_KEY", "sk-test"),
        ]))
        .expect("config");

        assert_eq!(config.provider, ProviderKind::OpenAi);
        assert_eq!(config.base_url, "https://api.openai.com/v1");
    }

    /// Google publishes its compatible surface at its own address, under a
    /// path that already contains the API version — so inheriting OpenAI's
    /// would fail in a way that names neither.
    #[test]
    fn google_brings_its_own_address_and_model() {
        let config = LlmConfig::from_vars(vars(&[
            ("ANAMNESIS_LLM_PROVIDER", "google"),
            ("GEMINI_API_KEY", "AQ.test-key"),
        ]))
        .expect("config");

        assert_eq!(config.provider, ProviderKind::Google);
        assert_eq!(
            config.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert!(config.model.starts_with("gemini"), "{}", config.model);
        assert!(config.build().expect("builds").is_some());
    }

    /// Four spellings, because this one backend is called three things by the
    /// people configuring it — the company, the product, and the console.
    #[test]
    fn google_answers_to_the_names_people_type() {
        for spelling in ["google", "gemini", "aistudio", "google-ai-studio", "GEMINI"] {
            let config = LlmConfig::from_vars(vars(&[
                ("ANAMNESIS_LLM_PROVIDER", spelling),
                ("GEMINI_API_KEY", "AQ.test-key"),
            ]))
            .unwrap_or_else(|error| panic!("{spelling} should configure: {error}"));

            assert_eq!(config.provider, ProviderKind::Google, "{spelling}");
        }
    }

    /// The rule that keeps a key from being a decision. Someone with a Gemini
    /// key exported for another tool has not asked this to summarise their
    /// sessions with it, and a provider that selected itself would send every
    /// transcript somewhere nobody chose.
    #[test]
    fn a_gemini_key_on_its_own_selects_nothing() {
        let config =
            LlmConfig::from_vars(vars(&[("GEMINI_API_KEY", "AQ.test-key")])).expect("config");

        assert_eq!(config.provider, ProviderKind::None);
        assert!(config.build().expect("builds").is_none());
    }

    /// Google asked for by name with nothing to authenticate with: the error
    /// names every variable that would have worked, because the one thing a
    /// person cannot guess is which spelling this reads.
    #[test]
    fn google_without_a_key_names_the_variables_it_looked_at() {
        let error = LlmConfig::from_vars(vars(&[("ANAMNESIS_LLM_PROVIDER", "google")]))
            .expect_err("should not configure");

        let message = error.to_string();
        for expected in ["GEMINI_API_KEY", "GOOGLE_API_KEY", "ANAMNESIS_LLM_API_KEY"] {
            assert!(message.contains(expected), "{message}");
        }
    }

    /// The per-provider default is a default, not a decision. Someone pointing
    /// at a gateway, a second Ollama on another port, or vLLM says so once.
    #[test]
    fn an_explicit_base_url_still_wins() {
        let config = LlmConfig::from_vars(vars(&[
            ("ANAMNESIS_LLM_PROVIDER", "ollama"),
            ("ANAMNESIS_LLM_BASE_URL", "http://gpu-box:8000/v1/"),
            ("ANAMNESIS_LLM_MODEL", "qwen2.5"),
        ]))
        .expect("config");

        assert_eq!(config.base_url, "http://gpu-box:8000/v1");
        assert_eq!(config.model, "qwen2.5");
    }

    /// A misspelled provider has to name what it could have been, or the
    /// person retyping it is guessing.
    #[test]
    fn an_unknown_provider_lists_the_ones_there_are() {
        let error = LlmConfig::from_vars(vars(&[("ANAMNESIS_LLM_PROVIDER", "openia")]))
            .expect_err("should refuse");
        let message = error.to_string();
        for expected in ["anthropic", "openai", "google", "ollama", "none"] {
            assert!(message.contains(expected), "{message}");
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

    #[test]
    fn a_waiting_caller_gets_the_short_retry_budget() {
        let config = LlmConfig::from_vars(|_| None).expect("defaults");

        assert_eq!(config.max_retries, DEFAULT_MAX_RETRIES);
    }

    #[test]
    fn work_nobody_waits_on_gets_a_longer_one() {
        let config =
            LlmConfig::from_vars_starting_at(BACKGROUND_MAX_RETRIES, |_| None).expect("defaults");

        assert!(
            config.max_retries > DEFAULT_MAX_RETRIES,
            "a summary nobody is waiting for should outlast a network coming up"
        );
        assert_eq!(config.max_retries, BACKGROUND_MAX_RETRIES);
    }

    /// An operator who has chosen a number keeps it, whichever budget the
    /// caller started from. Otherwise the setting would silently mean
    /// different things in `serve` and in `reconsolidate`.
    #[test]
    fn an_explicit_setting_wins_over_either_default() {
        for start in [DEFAULT_MAX_RETRIES, BACKGROUND_MAX_RETRIES] {
            let config = LlmConfig::from_vars_starting_at(start, |key| {
                (key == "ANAMNESIS_LLM_MAX_RETRIES").then(|| "1".to_owned())
            })
            .expect("explicit retries");

            assert_eq!(config.max_retries, 1, "starting from {start}");
        }
    }

    /// Nothing else about the two paths differs. If a later edit gives them
    /// separate defaults for anything, this says so.
    #[test]
    fn the_two_budgets_differ_in_nothing_but_the_budget() {
        let waiting = LlmConfig::from_vars(|_| None).expect("defaults");
        let unhurried =
            LlmConfig::from_vars_starting_at(BACKGROUND_MAX_RETRIES, |_| None).expect("defaults");

        assert_eq!(waiting.provider, unhurried.provider);
        assert_eq!(waiting.model, unhurried.model);
        assert_eq!(waiting.base_url, unhurried.base_url);
        assert_eq!(waiting.timeout, unhurried.timeout);
        assert_eq!(waiting.max_input_tokens, unhurried.max_input_tokens);
        assert_eq!(waiting.max_output_tokens, unhurried.max_output_tokens);
    }
}
