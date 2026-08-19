//! The Anthropic Messages API, as much of it as consolidation needs.
//!
//! Which is very little: one non-streaming POST per finished session, asking
//! for JSON in a shape we specify. Streaming exists for responses a human
//! watches arrive; nobody watches this one, and a two-thousand-token reply is
//! nowhere near the size where a non-streaming request risks a timeout.
//!
//! The request-building and response-reading halves are plain functions over
//! `serde_json::Value` so they can be tested without a network. That is where
//! the bugs in an API client actually live — a header spelled wrong, a content
//! block read by position instead of by type — and none of them need a socket
//! to find.

use std::time::Duration;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

use crate::LlmError;
use crate::config::LlmConfig;
use crate::provider::{Completion, CompletionOutput, Provider};

/// API version. Pinned, not tracked: this is the version the request shape
/// below was written against.
const API_VERSION: &str = "2023-06-01";

/// Beta flag that goes with the scalar `"fallbacks": "default"` form. Pairing
/// it with the array form is a 400, so the two travel together or not at all.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// A client for the Anthropic Messages API.
pub struct Anthropic {
    http: reqwest::Client,
    endpoint: String,
    api_key: secrecy::SecretString,
    model: String,
    effort: crate::config::Effort,
    max_retries: u32,
    server_side_fallbacks: bool,
}

impl std::fmt::Debug for Anthropic {
    /// Hand-written so no future `{:?}` on a config, a state struct, or an
    /// error can print the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Anthropic {
    /// Build a client from configuration.
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .api_key
            .clone()
            .ok_or_else(|| LlmError::Config("anthropic provider needs an api key".to_owned()))?;

        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()?;

        Ok(Self {
            http,
            endpoint: format!("{}/v1/messages", config.base_url),
            api_key,
            model: config.model.clone(),
            effort: config.effort,
            max_retries: config.max_retries,
            server_side_fallbacks: config.server_side_fallbacks,
        })
    }

    /// One attempt, no retry logic.
    async fn attempt(&self, body: &Value) -> Result<CompletionOutput, LlmError> {
        let mut request = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("anthropic-version", API_VERSION)
            .header("x-api-key", self.api_key.expose_secret());

        if self.server_side_fallbacks {
            request = request.header("anthropic-beta", FALLBACK_BETA);
        }

        let response = request.json(body).send().await?;
        let status = response.status();

        // The delay has to be read before the body is consumed, and it is the
        // only trustworthy source for how long a 429 wants us to wait.
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);

        let text = response.text().await?;

        if !status.is_success() {
            return Err(api_error(status.as_u16(), &text, retry_after));
        }

        let payload: Value = serde_json::from_str(&text)
            .map_err(|error| LlmError::Malformed(format!("response was not JSON: {error}")))?;

        parse_response(&payload)
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError> {
        let body = build_body(
            &self.model,
            self.effort,
            self.server_side_fallbacks,
            request,
        );

        let mut attempt = 0;
        loop {
            match self.attempt(&body).await {
                Ok(output) => return Ok(output),
                Err(error) if attempt < self.max_retries && error.is_retryable() => {
                    let delay = retry_delay(&error, attempt);
                    tracing::warn!(
                        attempt = attempt + 1,
                        delay_ms = delay.as_millis(),
                        %error,
                        "llm request failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Assemble the request body.
///
/// Thinking is left unset on purpose. On the current Opus generation it is on
/// by default and `budget_tokens` is gone, so naming it here would either be
/// a no-op or — with a stale value copied from older code — a 400.
fn build_body(
    model: &str,
    effort: crate::config::Effort,
    server_side_fallbacks: bool,
    request: &Completion,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": request.max_output_tokens,
        "system": request.system,
        "messages": [{
            "role": "user",
            "content": request.user,
        }],
        "output_config": {
            "effort": effort.as_str(),
            "format": {
                "type": "json_schema",
                "schema": request.schema,
            },
        },
    });

    if server_side_fallbacks {
        // A session transcript is arbitrary text someone else wrote. If a
        // classifier declines it, having the API re-run the same request on
        // another model is strictly better than losing the summary — and the
        // deterministic page is still there if the whole chain declines.
        body["fallbacks"] = json!("default");
    }

    body
}

/// Turn a successful response into an answer, or explain why it is not one.
fn parse_response(payload: &Value) -> Result<CompletionOutput, LlmError> {
    let stop_reason = payload
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if stop_reason == "refusal" {
        let category = payload
            .get("stop_details")
            .and_then(|details| details.get("category"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Err(LlmError::Refused { category });
    }

    if stop_reason == "max_tokens" {
        // The JSON is cut off mid-object, so parsing it would fail anyway —
        // but with a message that blames the model's grammar rather than our
        // budget, which is the thing someone would have to fix.
        return Err(LlmError::Malformed(
            "reply hit max_tokens and is incomplete; raise ANAMNESIS_LLM_MAX_OUTPUT_TOKENS"
                .to_owned(),
        ));
    }

    // Content is a list of typed blocks and the first one is not necessarily
    // the answer — with thinking on, it is a thinking block. Selecting by type
    // is the only stable way to find the text.
    let text = payload
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| LlmError::Malformed("response carried no text block".to_owned()))?;

    let json: Value = serde_json::from_str(text.trim())
        .map_err(|error| LlmError::Malformed(format!("reply was not the JSON we asked for: {error}")))?;

    Ok(CompletionOutput {
        json,
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        input_tokens: usage(payload, "input_tokens"),
        output_tokens: usage(payload, "output_tokens"),
    })
}

/// One usage counter, defaulting to zero rather than failing the request over
/// a missing accounting field.
fn usage(payload: &Value, field: &str) -> u32 {
    payload
        .get("usage")
        .and_then(|usage| usage.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Classify a non-2xx response.
fn api_error(status: u16, body: &str, retry_after: Option<Duration>) -> LlmError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let kind = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(body)
        .to_owned();

    // Folded into the message rather than a field: the only consumer is a log
    // line and the retry loop, and the loop reads it back below.
    let message = match retry_after {
        Some(delay) => format!("{message} (retry after {}s)", delay.as_secs()),
        None => message,
    };

    LlmError::Api {
        status,
        kind,
        message,
    }
}

/// How long to wait before trying again.
///
/// Honours a `retry-after` the API sent, and otherwise backs off
/// exponentially from a second. Capped, because the session is already over
/// and a page that arrives ten minutes late is worth less than the process
/// being free to handle the next one.
fn retry_delay(error: &LlmError, attempt: u32) -> Duration {
    if let LlmError::Api { message, .. } = error
        && let Some(seconds) = message
            .rsplit_once("(retry after ")
            .and_then(|(_, rest)| rest.split_once("s)"))
            .and_then(|(seconds, _)| seconds.parse::<u64>().ok())
    {
        return Duration::from_secs(seconds.min(60));
    }

    Duration::from_secs(2_u64.saturating_pow(attempt).min(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn request() -> Completion {
        Completion {
            system: "you summarise sessions".to_owned(),
            user: "a session happened".to_owned(),
            schema: json!({"type": "object"}),
            max_output_tokens: 2_000,
        }
    }

    #[test]
    fn the_body_asks_for_json_in_our_schema() {
        let body = build_body("claude-opus-5", Effort::High, false, &request());
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["max_tokens"], 2_000);
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn thinking_is_never_named_in_the_body() {
        // Sending `budget_tokens` to a current Opus model is a 400, and the
        // safest way to never send it is to never send `thinking` at all.
        let body = build_body("claude-opus-5", Effort::High, true, &request());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn fallbacks_appear_only_when_enabled() {
        let with = build_body("claude-opus-5", Effort::High, true, &request());
        assert_eq!(with["fallbacks"], "default");
        let without = build_body("claude-opus-5", Effort::High, false, &request());
        assert!(without.get("fallbacks").is_none());
    }

    #[test]
    fn the_text_block_is_found_by_type_not_by_position() {
        // A thinking block ahead of the answer is the normal shape of a
        // response from a model with thinking on; reading content[0] would
        // find it and fail on every single request.
        let payload = json!({
            "model": "claude-opus-5",
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"title\":\"t\"}"},
            ],
            "usage": {"input_tokens": 12, "output_tokens": 34},
        });
        let output = parse_response(&payload).expect("parses");
        assert_eq!(output.json["title"], "t");
        assert_eq!(output.input_tokens, 12);
        assert_eq!(output.output_tokens, 34);
    }

    #[test]
    fn the_model_that_answered_is_recorded_not_the_one_we_asked_for() {
        let payload = json!({
            "model": "claude-opus-4-8",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{}"}],
        });
        let output = parse_response(&payload).expect("parses");
        assert_eq!(output.model, "claude-opus-4-8");
    }

    #[test]
    fn a_refusal_is_its_own_error_and_is_not_retried() {
        let payload = json!({
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber"},
            "content": [],
        });
        let error = parse_response(&payload).expect_err("refused");
        assert!(matches!(error, LlmError::Refused { .. }));
        assert!(!error.is_retryable());
        assert!(error.to_string().contains("cyber"));
    }

    #[test]
    fn a_truncated_reply_says_which_setting_to_raise() {
        let payload = json!({
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": "{\"title\": \"half a "}],
        });
        let error = parse_response(&payload).expect_err("truncated");
        assert!(error.to_string().contains("ANAMNESIS_LLM_MAX_OUTPUT_TOKENS"));
    }

    #[test]
    fn a_reply_with_no_text_block_is_malformed() {
        let payload = json!({"stop_reason": "end_turn", "content": []});
        assert!(matches!(
            parse_response(&payload),
            Err(LlmError::Malformed(_))
        ));
    }

    #[test]
    fn prose_where_json_was_asked_for_is_malformed_not_a_panic() {
        let payload = json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Sure! Here is your summary:"}],
        });
        assert!(matches!(
            parse_response(&payload),
            Err(LlmError::Malformed(_))
        ));
    }

    #[test]
    fn api_errors_carry_their_type_and_message() {
        let error = api_error(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad model"}}"#,
            None,
        );
        match &error {
            LlmError::Api {
                status,
                kind,
                message,
            } => {
                assert_eq!(*status, 400);
                assert_eq!(kind, "invalid_request_error");
                assert_eq!(message, "bad model");
            }
            other => panic!("expected an api error, got {other:?}"),
        }
        assert!(!error.is_retryable());
    }

    #[test]
    fn a_non_json_error_body_is_still_reported() {
        let error = api_error(502, "<html>bad gateway</html>", None);
        assert!(error.to_string().contains("bad gateway"));
        assert!(error.is_retryable());
    }

    #[test]
    fn rate_limits_and_server_faults_are_retryable() {
        for status in [429, 500, 529] {
            assert!(api_error(status, "{}", None).is_retryable(), "{status}");
        }
        for status in [400, 401, 403, 404, 413] {
            assert!(!api_error(status, "{}", None).is_retryable(), "{status}");
        }
    }

    #[test]
    fn a_retry_after_header_wins_over_the_backoff_curve() {
        let error = api_error(429, "{}", Some(Duration::from_secs(7)));
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(7));
    }

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        let error = api_error(500, "{}", None);
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(&error, 2), Duration::from_secs(4));
        assert_eq!(retry_delay(&error, 20), Duration::from_secs(30));
    }
}
