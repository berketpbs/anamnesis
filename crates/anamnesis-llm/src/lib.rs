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
mod config;
pub mod embed;
pub mod hosted;
mod http;
mod openai;
mod provider;

pub use anthropic::Anthropic;
pub use budget::{clip_to_tokens, estimate_tokens};
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

    /// The request never reached a verdict: connection refused, DNS, timeout.
    #[error("llm transport failed: {0}")]
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

impl LlmError {
    /// Whether trying the same request again could plausibly succeed.
    ///
    /// Rate limits and server faults are transient; a bad key or a refusal is
    /// not, and retrying those only delays the fallback.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(error) => {
                error.is_timeout() || error.is_connect() || error.is_request()
            }
            Self::Api { status, .. } => matches!(status, 408 | 409 | 429) || *status >= 500,
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
}
