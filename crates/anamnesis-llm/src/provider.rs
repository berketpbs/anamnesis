//! The shape of a model, as far as anamnesis is concerned.

use async_trait::async_trait;

use crate::LlmError;

/// One question for a model.
///
/// The schema is not optional. Consolidation needs three named fields back,
/// and asking for them in prose means writing a parser for whatever the model
/// felt like emitting — including the day it decides to wrap the answer in a
/// friendly paragraph. Constraining the output at the API level removes that
/// entire class of failure.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Instructions that describe the job, stable across requests.
    ///
    /// Kept separate from `user` because it is the same for every session,
    /// which is what makes it worth caching on providers that support it.
    pub system: String,

    /// The material to work from — this session, and nothing else.
    pub user: String,

    /// JSON Schema the reply has to satisfy.
    pub schema: serde_json::Value,

    /// Ceiling on the reply. A consolidation that wants more than this has
    /// misunderstood the job.
    pub max_output_tokens: u32,
}

/// What a model gave back, once it was known to be usable.
#[derive(Debug, Clone)]
pub struct CompletionOutput {
    /// The reply, parsed. Guaranteed to be JSON; not guaranteed to contain
    /// the fields the caller wanted — that check belongs to the caller, which
    /// is the only place that knows what they mean.
    pub json: serde_json::Value,

    /// Which model actually answered. Worth recording: with server-side
    /// fallbacks enabled this is not always the model that was asked.
    pub model: String,

    /// Tokens billed on the way in.
    pub input_tokens: u32,

    /// Tokens billed on the way out.
    pub output_tokens: u32,

    /// The model that was asked first and did not answer, when a fallback did.
    ///
    /// `None` on every reply the configured model gave. Set only by a
    /// [`Chain`](crate::Chain), because only the chain knows the reply came from
    /// somewhere other than what was configured — and a page written by a
    /// weaker stand-in that does not say so is the silent substitution this
    /// project exists to prevent.
    pub instead_of: Option<String>,
}

/// A language model anamnesis can ask for one structured answer.
///
/// Async because the implementations are HTTP clients and the consolidation
/// path has somewhere better to be while a request is in flight. Implementors
/// are shared across sessions, hence `Send + Sync`.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Which backend this is, for logs.
    fn name(&self) -> &'static str;

    /// The model that will be asked, for logs and for the page footer.
    fn model(&self) -> &str;

    /// One line naming what will be asked, for a banner a person reads.
    ///
    /// Separate from [`Provider::model`], which is the one model a reply is
    /// attributed to: a chain asks several, and a banner that named only the
    /// first would hide the rest from the one person who configured them.
    fn describe(&self) -> String {
        format!("{} ({})", self.model(), self.name())
    }

    /// Ask once. Retries, if any, are the implementation's business.
    async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError>;
}
