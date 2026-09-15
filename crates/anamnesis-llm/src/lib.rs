//! Talking to a language model, for the one job anamnesis has for one.
//!
//! Consolidation without a model already works — [`anamnesis_consolidate`]
//! counts what happened and says so. What a model adds is the part counting
//! cannot reach: *why* a session did what it did. That is the whole reason
//! this crate exists, and it is why every path through it is optional. A
//! missing key, a refused request, a 500, a reply that does not match the
//! schema — each one has to degrade to the deterministic page rather than
//! cost someone their session summary.
//!
//! The provider abstraction is deliberately narrow: one request, one JSON
//! reply, shaped by a schema the caller supplies. No streaming, no tools, no
//! conversation. Consolidation output is small and nobody is watching it
//! arrive, so the machinery those features need would be pure liability here.
//!
//! [`anamnesis_consolidate`]: https://docs.rs/anamnesis-consolidate

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod anthropic;
mod budget;
mod chain;
mod config;
pub mod embed;
pub mod hosted;
mod http;
mod openai;
mod provider;

pub use anthropic::Anthropic;
pub use budget::{clip_to_tokens, estimate_tokens};
pub use chain::Chain;
pub use config::{Effort, LlmConfig, ProviderKind, provider_from_env};
pub use embed::{EmbedConfig, EmbedError, Embedder, LocalEmbedder};
pub use openai::OpenAiCompatible;
pub use provider::{Completion, CompletionOutput, Provider};

/// Everything that can go wrong between asking a model and having an answer.
///
/// The variants exist to be *classified*, not printed: the caller needs to
/// know whether waiting would help ([`LlmError::is_retryable`]) and whether
/// the deterministic fallback should take over (always, in practice).
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The provider was asked for but not usable — no key, unknown name, a
    /// setting that does not parse.
    #[error("llm is misconfigured: {0}")]
    Config(String),

    /// The request never reached a verdict: connection refused, DNS, timeout,
    /// or a reply that stopped arriving part way through.
    ///
    /// Printed with what caused it. reqwest's own sentence is the outermost
    /// one only, and on 2026-09-13 the log said `error decoding response body`
    /// twice and nothing about why.
    #[error("llm transport failed: {}", with_causes(.0))]
    Transport(#[from] reqwest::Error),

    /// The API answered, and the answer was an error.
    #[error("llm api error {status} ({kind}): {message}")]
    Api {
        /// HTTP status.
        status: u16,
        /// The `error.type` the API reported, or `unknown`.
        kind: String,
        /// The human-readable message.
        message: String,
    },

    /// The model ran out of output budget mid-answer.
    ///
    /// Separate from [`Self::Malformed`] because the caller can do something
    /// about this one and nothing about the other. A reply that is bad JSON
    /// will be bad again on the same request; a reply that did not fit can be
    /// asked for smaller, and a caller that knows which part of its request
    /// was optional is the only thing that knows how.
    #[error("llm reply did not fit its output budget: {0}")]
    Truncated(String),

    /// A safety classifier declined the request. Not a failure of ours, and
    /// not something a retry fixes.
    #[error("llm declined the request{}", match .category {
        Some(c) => format!(" ({c})"),
        None => String::new(),
    })]
    Refused {
        /// The refusal category the API reported, when it named one.
        category: Option<String>,
    },

    /// A 200 that we could not use: no text block, invalid JSON, or JSON that
    /// does not fit the schema we asked for.
    #[error("llm reply was unusable: {0}")]
    Malformed(String),
}

/// An error's sentence followed by each cause's, as `a: b: c`.
///
/// A cause that only repeats the sentence before it is left out.
fn with_causes(error: &(dyn std::error::Error + 'static)) -> String {
    let mut said = error.to_string();
    let mut last = said.clone();
    let mut cause = error.source();
    while let Some(next) = cause {
        let sentence = next.to_string();
        if !last.contains(&sentence) {
            said.push_str(": ");
            said.push_str(&sentence);
        }
        last = sentence;
        cause = next.source();
    }
    said
}

impl LlmError {
    /// Whether trying the same request again could plausibly succeed.
    ///
    /// Rate limits and server faults are transient; a bad key or a refusal is
    /// not, and retrying those only delays the fallback.
    ///
    /// A quota spent for the day is not: see [`LlmError::is_spent_for_the_day`].
    pub fn is_retryable(&self) -> bool {
        match self {
            // A body that stopped arriving is a dropped connection that
            // happened later: the model answered, and the answer did not get
            // here. Twice on 2026-09-13 that cost a session its model page on
            // the first attempt, while a timeout at the same moment would have
            // been asked again.
            Self::Transport(error) => {
                error.is_timeout()
                    || error.is_connect()
                    || error.is_request()
                    || error.is_body()
                    || error.is_decode()
            }
            Self::Api { status, .. } => {
                (matches!(status, 408 | 409 | 429) || *status >= 500)
                    && !self.is_spent_for_the_day()
            }
            // A malformed reply is worth one more roll of the dice: sampling
            // is not deterministic, and the same prompt often parses next time.
            Self::Malformed(_) => true,
            // Measured, not reasoned about: the same request truncated on
            // every attempt, twice over, in two separate runs. Nothing about
            // a ceiling changes between one request and the next, so a retry
            // here is a refusal bought at full price — six requests where two
            // were possible. The caller is the one that can act on this, by
            // asking for less.
            Self::Truncated(_) => false,
            Self::Config(_) | Self::Refused { .. } => false,
        }
    }

    /// Whether this is a refusal for a quota counted per day.
    ///
    /// A 429 like any other, and the one a retry cannot outlast: Google's
    /// free tier allows twenty requests a day per model, the refusal asks for
    /// half a minute, and the retry loop — eight attempts for the server's
    /// background work — spent about five minutes per session re-asking a
    /// model that would not answer before midnight Pacific. So it is not
    /// retried. It is still handed on by a [`Chain`]: the quota is the
    /// model's, and the next model on the same key has its own.
    ///
    /// Read from the `[quotaId]` the HTTP layer appends when the body
    /// names one — `GenerateRequestsPerDayPerProjectPerModel-FreeTier`,
    /// `GenerateContentInputTokensPerModelPerDay-FreeTier` — since the status,
    /// the kind and the sentence are the same for a per-minute limit.
    pub fn is_spent_for_the_day(&self) -> bool {
        matches!(
            self,
            Self::Api { status: 429, message, .. }
                if message
                    .split('[')
                    .skip(1)
                    .filter_map(|rest| rest.split_once(']'))
                    .any(|(quota, _)| quota.contains("PerDay"))
        )
    }
}
