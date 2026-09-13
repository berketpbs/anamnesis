//! Providers tried in order, for the afternoons the first one does not answer.
//!
//! The evidence for this is this project's own. On 2026-09-07 two Gemini flash
//! models answered `503 high demand` to every real consolidation request for an
//! afternoon, while a third served the same session in the same minutes and a
//! local `qwen2.5:7b-instruct` wrote it in sixteen seconds with no key and no
//! quota. On 2026-09-13 a measurement spent the day's free quota by midday. Each
//! time the server did exactly what it was built to do — it fell back to the
//! counted summary — and each of those sessions became a tally where a slightly
//! worse page was one request away.
//!
//! Two rules keep the chain from becoming the failure it covers for.
//!
//! **Only a transient failure is handed on.** A timeout, a refused connection,
//! a rate limit, a server fault, a reply that did not parse — the things
//! [`LlmError::is_retryable`] already names, after the link's own retries are
//! spent. A bad key, an unknown model, a refusal and a reply that did not fit
//! stop the chain where they happened: the first two are configuration someone
//! has to see, and a chain that routes around them hides them for good; sending
//! a transcript one provider declined to a second is a decision nobody made;
//! and a reply that did not fit is the caller's to ask for smaller.
//!
//! **A reply from a stand-in says so.** [`CompletionOutput::instead_of`] names
//! the model that was asked first, and the consolidator puts that on the page.

use std::sync::Arc;

use async_trait::async_trait;

use crate::LlmError;
use crate::provider::{Completion, CompletionOutput, Provider};

/// An ordered list of providers, asked one after another on transient failure.
pub struct Chain {
    links: Vec<Arc<dyn Provider>>,
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain")
            .field("links", &self.describe())
            .finish()
    }
}

impl Chain {
    /// A chain over `links`, asked in the order given.
    ///
    /// # Panics
    ///
    /// When `links` is empty. A chain of nothing has no model to name, and the
    /// configuration that would produce one is refused before it gets here.
    #[must_use]
    pub fn new(links: Vec<Arc<dyn Provider>>) -> Self {
        assert!(!links.is_empty(), "a chain needs at least one provider");
        Self { links }
    }

    fn first(&self) -> &dyn Provider {
        self.links[0].as_ref()
    }
}

/// Whether the next link should be asked after this failure.
fn hands_on(error: &LlmError) -> bool {
    error.is_retryable()
}

#[async_trait]
impl Provider for Chain {
    /// The configured provider's name. A reply from a stand-in carries the
    /// stand-in's model in [`CompletionOutput::model`] and the configured one
    /// in [`CompletionOutput::instead_of`].
    fn name(&self) -> &'static str {
        self.first().name()
    }

    fn model(&self) -> &str {
        self.first().model()
    }

    fn describe(&self) -> String {
        self.links
            .iter()
            .map(|link| link.describe())
            .collect::<Vec<_>>()
            .join(", then ")
    }

    async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError> {
        let last = self.links.len() - 1;
        for (index, link) in self.links.iter().enumerate() {
            match link.complete(request).await {
                Ok(mut output) => {
                    if index > 0 {
                        output.instead_of = Some(self.first().model().to_owned());
                    }
                    return Ok(output);
                }
                Err(error) if index < last && hands_on(&error) => {
                    let next = &self.links[index + 1];
                    tracing::warn!(
                        provider = link.name(),
                        model = link.model(),
                        next_provider = next.name(),
                        next_model = next.model(),
                        %error,
                        "the model did not answer; asking the next one in the chain"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("the last link either answers or returns its error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// A link that answers with whatever it was given, and counts its calls.
    struct Link {
        name: &'static str,
        model: &'static str,
        answer: fn() -> Result<CompletionOutput, LlmError>,
        asked: Mutex<usize>,
    }

    impl Link {
        fn new(
            name: &'static str,
            model: &'static str,
            answer: fn() -> Result<CompletionOutput, LlmError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                name,
                model,
                answer,
                asked: Mutex::new(0),
            })
        }
        fn asked(&self) -> usize {
            *self.asked.lock().expect("lock")
        }
    }

    #[async_trait]
    impl Provider for Link {
        fn name(&self) -> &'static str {
            self.name
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(&self, _: &Completion) -> Result<CompletionOutput, LlmError> {
            *self.asked.lock().expect("lock") += 1;
            (self.answer)()
        }
    }

    fn request() -> Completion {
        Completion {
            system: "s".to_owned(),
            user: "u".to_owned(),
            schema: json!({"type": "object"}),
            max_output_tokens: 1_000,
        }
    }

    fn answered() -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput {
            json: json!({"title": "t"}),
            model: "answering-model".to_owned(),
            input_tokens: 1,
            output_tokens: 1,
            instead_of: None,
        })
    }

    fn overloaded() -> Result<CompletionOutput, LlmError> {
        Err(LlmError::Api {
            status: 503,
            kind: "unavailable".to_owned(),
            message: "high demand".to_owned(),
        })
    }

    fn out_of_quota() -> Result<CompletionOutput, LlmError> {
        Err(LlmError::Api {
            status: 429,
            kind: "RESOURCE_EXHAUSTED".to_owned(),
            message: "quota exceeded".to_owned(),
        })
    }

    fn bad_key() -> Result<CompletionOutput, LlmError> {
        Err(LlmError::Api {
            status: 401,
            kind: "authentication_error".to_owned(),
            message: "invalid x-api-key".to_owned(),
        })
    }

    fn declined() -> Result<CompletionOutput, LlmError> {
        Err(LlmError::Refused { category: None })
    }

    fn did_not_fit() -> Result<CompletionOutput, LlmError> {
        Err(LlmError::Truncated("did not fit".to_owned()))
    }

    #[tokio::test]
    async fn the_configured_model_answers_and_nobody_else_is_asked() {
        let first = Link::new("google", "gemini-3.5-flash", answered);
        let second = Link::new("ollama", "qwen2.5:7b-instruct", answered);
        let chain = Chain::new(vec![first.clone(), second.clone()]);

        let output = chain.complete(&request()).await.expect("an answer");

        assert_eq!(output.instead_of, None, "no stand-in answered");
        assert_eq!((first.asked(), second.asked()), (1, 0));
    }

    /// The afternoon of 2026-09-07, with the chain that would have kept it.
    #[tokio::test]
    async fn an_overloaded_model_hands_on_and_the_reply_names_what_it_replaced() {
        let first = Link::new("google", "gemini-3.8-flash", overloaded);
        let second = Link::new("ollama", "qwen2.5:7b-instruct", answered);
        let chain = Chain::new(vec![first.clone(), second.clone()]);

        let output = chain.complete(&request()).await.expect("an answer");

        assert_eq!(output.instead_of.as_deref(), Some("gemini-3.8-flash"));
        assert_eq!((first.asked(), second.asked()), (1, 1));
    }

    #[tokio::test]
    async fn a_spent_quota_hands_on_past_every_link_that_is_out() {
        let first = Link::new("google", "gemini-3.5-flash", out_of_quota);
        let second = Link::new("google", "gemini-3.6-flash", out_of_quota);
        let third = Link::new("ollama", "qwen2.5:7b-instruct", answered);
        let chain = Chain::new(vec![first, second, third.clone()]);

        let output = chain.complete(&request()).await.expect("an answer");

        assert_eq!(
            output.instead_of.as_deref(),
            Some("gemini-3.5-flash"),
            "named after the configured model, not the link before the one that answered"
        );
        assert_eq!(third.asked(), 1);
    }

    /// The last link's error is the one returned: it is what the caller's
    /// log line will say, and the earlier ones were each logged as they
    /// handed on.
    #[tokio::test]
    async fn a_chain_where_nobody_answers_returns_the_last_failure() {
        let first = Link::new("google", "gemini-3.5-flash", out_of_quota);
        let second = Link::new("ollama", "qwen2.5:7b-instruct", overloaded);
        let chain = Chain::new(vec![first, second]);

        let error = chain.complete(&request()).await.expect_err("no answer");

        assert!(
            matches!(error, LlmError::Api { status: 503, .. }),
            "{error}"
        );
    }

    /// Configuration somebody has to see. Routing around a bad key would keep
    /// every page coming from the stand-in, and nobody would ever learn the
    /// configured model had stopped being asked.
    #[tokio::test]
    async fn a_bad_key_stops_the_chain_where_it_happened() {
        let second = Link::new("ollama", "qwen2.5:7b-instruct", answered);
        let chain = Chain::new(vec![
            Link::new("anthropic", "claude-opus-5", bad_key),
            second.clone(),
        ]);

        let error = chain.complete(&request()).await.expect_err("refused");

        assert!(matches!(error, LlmError::Api { status: 401, .. }));
        assert_eq!(second.asked(), 0);
    }

    /// Sending a transcript one provider declined on to another is a decision,
    /// and not one anybody configuring a fallback for outages has made.
    #[tokio::test]
    async fn a_refusal_is_not_shopped_around() {
        let second = Link::new("ollama", "qwen2.5:7b-instruct", answered);
        let chain = Chain::new(vec![
            Link::new("anthropic", "claude-opus-5", declined),
            second.clone(),
        ]);

        assert!(chain.complete(&request()).await.is_err());
        assert_eq!(second.asked(), 0);
    }

    /// The consolidator answers a reply that did not fit by asking for less;
    /// a chain that passed it to a model with a smaller window would take that
    /// away.
    #[tokio::test]
    async fn a_reply_that_did_not_fit_goes_back_to_the_caller() {
        let second = Link::new("ollama", "qwen2.5:7b-instruct", answered);
        let chain = Chain::new(vec![
            Link::new("google", "gemini-3.5-flash", did_not_fit),
            second.clone(),
        ]);

        let error = chain.complete(&request()).await.expect_err("truncated");

        assert!(matches!(error, LlmError::Truncated(_)));
        assert_eq!(second.asked(), 0);
    }

    #[test]
    fn the_chain_is_attributed_to_its_first_link_and_described_whole() {
        let chain = Chain::new(vec![
            Link::new("google", "gemini-3.5-flash", answered),
            Link::new("ollama", "qwen2.5:7b-instruct", answered),
        ]);

        assert_eq!(chain.name(), "google");
        assert_eq!(chain.model(), "gemini-3.5-flash");
        assert_eq!(
            chain.describe(),
            "gemini-3.5-flash (google), then qwen2.5:7b-instruct (ollama)"
        );
    }
}
