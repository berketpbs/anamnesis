//! What the model said the last time it did not answer.
//!
//! `status` reads what the configured model has been producing from the
//! sessions themselves, and on 2026-09-14 that line was right: "the last 4
//! were counted, the model is not answering". It was also where the trail
//! stopped. The reason — `400 Please pass a valid API key` — was in the
//! server's log, on the fifth line of an entry repeated every minute, and the
//! next step after reading `status` was to guess between a revoked key, a
//! spent quota and an overloaded model, which call for three different things.
//!
//! The server is the only process that asks the model, so it is the only one
//! that can say. [`Watched`] sits around the configured provider, remembers
//! the last failure, and forgets it the next time a request is answered: what
//! it reports is a fault that is still the latest word, never one a later
//! success has already outlived. Held in memory, because a restart is what
//! somebody does after fixing a key, and a restarted server that went on
//! naming the old refusal would contradict the fix.

use std::sync::Arc;

use anamnesis_core::sanitize::Redactor;
use anamnesis_llm::{Completion, CompletionOutput, LlmError, Provider};
use async_trait::async_trait;
use jiff::Timestamp;
use parking_lot::Mutex;
use serde::Serialize;

/// Longest reason kept, in characters. One line of `status`.
const MAX_REASON_CHARS: usize = 300;

/// One request the model did not answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelFailure {
    /// When the refusal came back.
    pub at: Timestamp,
    /// The HTTP status, when the provider answered with one.
    pub status: Option<u16>,
    /// What went wrong, as a clause that follows "the model": `answered 400:
    /// Please pass a valid API key`, `could not be reached: ...`.
    pub reason: String,
}

impl ModelFailure {
    /// The failure an error amounts to, at `at`.
    ///
    /// Redacted before it is kept: a provider's message is somebody else's
    /// text, and a gateway that echoes the request's headers back would
    /// otherwise put the key in `/whoami`.
    pub fn from_error(error: &LlmError, at: Timestamp) -> Self {
        let (status, reason) = match error {
            LlmError::Api {
                status, message, ..
            } => {
                // The wait is the retry loop's business, and it was spent.
                let message = message
                    .rsplit_once(" (retry after ")
                    .map_or(message.as_str(), |(head, _)| head);
                (Some(*status), format!("answered {status}: {message}"))
            }
            LlmError::Transport(error) => (None, format!("could not be reached: {error}")),
            LlmError::Config(message) => (None, format!("is misconfigured: {message}")),
            LlmError::Truncated(message) => (None, format!("did not fit its reply: {message}")),
            LlmError::Refused { category } => (
                None,
                match category {
                    Some(category) => format!("declined the request ({category})"),
                    None => "declined the request".to_owned(),
                },
            ),
            LlmError::Malformed(message) => {
                (None, format!("answered with something unusable: {message}"))
            }
        };
        let reason = Redactor::new().redact(&reason).into_text();
        let reason = match reason.char_indices().nth(MAX_REASON_CHARS) {
            Some((end, _)) => format!("{}…", &reason[..end]),
            None => reason,
        };
        Self { at, status, reason }
    }
}

/// The last failure a [`Watched`] provider saw, shared with whoever reports it.
#[derive(Debug, Clone, Default)]
pub struct LastFailure(Arc<Mutex<Option<ModelFailure>>>);

impl LastFailure {
    /// The failure, when the latest request was not answered.
    pub fn get(&self) -> Option<ModelFailure> {
        self.0.lock().clone()
    }

    fn set(&self, failure: Option<ModelFailure>) {
        *self.0.lock() = failure;
    }
}

/// A provider that remembers how its last request went.
pub struct Watched {
    inner: Arc<dyn Provider>,
    last: LastFailure,
}

impl Watched {
    /// Watch `inner`, reporting into `last`.
    pub fn new(inner: Arc<dyn Provider>, last: LastFailure) -> Self {
        Self { inner, last }
    }
}

#[async_trait]
impl Provider for Watched {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }

    async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError> {
        let answer = self.inner.complete(request).await;
        match &answer {
            Ok(_) => self.last.set(None),
            Err(error) => self
                .last
                .set(Some(ModelFailure::from_error(error, Timestamp::now()))),
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Answers from a list, one per request, and then the last one forever.
    struct Scripted(Mutex<Vec<Result<CompletionOutput, LlmError>>>);

    #[async_trait]
    impl Provider for Scripted {
        fn name(&self) -> &'static str {
            "google"
        }
        fn model(&self) -> &str {
            "gemini-3.5-flash"
        }
        async fn complete(&self, _: &Completion) -> Result<CompletionOutput, LlmError> {
            let mut script = self.0.lock();
            if script.len() > 1 {
                script.remove(0)
            } else {
                match &script[0] {
                    Ok(output) => Ok(output.clone()),
                    Err(_) => Err(LlmError::Config("again".to_owned())),
                }
            }
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
            json: json!({}),
            model: "gemini-3.5-flash".to_owned(),
            input_tokens: 1,
            output_tokens: 1,
            instead_of: None,
        })
    }

    fn bad_key() -> Result<CompletionOutput, LlmError> {
        Err(LlmError::Api {
            status: 400,
            kind: "INVALID_ARGUMENT".to_owned(),
            message: "Please pass a valid API key".to_owned(),
        })
    }

    /// Remembered when it happens, forgotten as soon as a request is answered:
    /// a reason `status` prints is always the latest word.
    #[tokio::test]
    async fn a_failure_is_kept_until_the_next_answer() {
        let last = LastFailure::default();
        let watched = Watched::new(
            Arc::new(Scripted(Mutex::new(vec![bad_key(), answered()]))),
            last.clone(),
        );
        assert_eq!(last.get(), None, "nothing asked, nothing to say");

        assert!(watched.complete(&request()).await.is_err());
        let failure = last.get().expect("the refusal is kept");
        assert_eq!(failure.status, Some(400));
        assert_eq!(failure.reason, "answered 400: Please pass a valid API key");

        assert!(watched.complete(&request()).await.is_ok());
        assert_eq!(last.get(), None, "an answer forgets it");
    }

    #[test]
    fn the_wait_is_left_off_and_a_quota_is_kept() {
        let failure = ModelFailure::from_error(
            &LlmError::Api {
                status: 429,
                kind: "RESOURCE_EXHAUSTED".to_owned(),
                message: "You exceeded your current quota. \
                          [GenerateRequestsPerDayPerProjectPerModel-FreeTier] (retry after 31s)"
                    .to_owned(),
            },
            Timestamp::UNIX_EPOCH,
        );
        assert_eq!(
            failure.reason,
            "answered 429: You exceeded your current quota. \
             [GenerateRequestsPerDayPerProjectPerModel-FreeTier]"
        );
    }

    /// Somebody else's text, kept where anyone who can reach `/whoami` reads
    /// it. A gateway that echoed the credential back must not publish it.
    #[test]
    fn a_key_in_the_message_is_redacted_before_it_is_kept() {
        let failure = ModelFailure::from_error(
            &LlmError::Api {
                status: 401,
                kind: "unknown".to_owned(),
                message: "rejected Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH"
                    .to_owned(),
            },
            Timestamp::UNIX_EPOCH,
        );
        assert!(
            !failure
                .reason
                .contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "{}",
            failure.reason
        );
        assert!(failure.reason.contains("redacted"), "{}", failure.reason);
    }

    #[test]
    fn a_reason_is_one_line_of_status_at_most() {
        let failure = ModelFailure::from_error(
            &LlmError::Malformed("x".repeat(5_000)),
            Timestamp::UNIX_EPOCH,
        );
        assert!(failure.reason.chars().count() <= MAX_REASON_CHARS + 1);
        assert!(failure.reason.ends_with('…'));
    }

    /// The watched provider is the provider, to everything that reads its
    /// name: pages are attributed to it and the banner describes it.
    #[test]
    fn watching_changes_nothing_a_reader_sees() {
        let watched = Watched::new(
            Arc::new(Scripted(Mutex::new(vec![answered()]))),
            LastFailure::default(),
        );
        assert_eq!(watched.name(), "google");
        assert_eq!(watched.model(), "gemini-3.5-flash");
        assert_eq!(watched.describe(), "gemini-3.5-flash (google)");
    }
}
