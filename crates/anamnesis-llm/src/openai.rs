//! The OpenAI chat-completions API, and everything that speaks it.
//!
//! One client, several backends, because they are one wire format: OpenAI
//! itself, Ollama, Google AI Studio, vLLM, LM Studio, OpenRouter, and any
//! gateway that presents `/chat/completions`. Writing an "Ollama provider" and
//! an "OpenAI provider" separately would be writing the same bug twice.
//!
//! What it needs from a backend is narrow — one non-streaming POST, a reply
//! constrained by a JSON schema — and that is the whole reason a local model
//! can be dropped in where a hosted one was. Verified against Ollama 0.32
//! before this was written, not assumed: `response_format` with a
//! `json_schema` is honoured there, and the reply came back with exactly the
//! properties the schema named.
//!
//! Two findings from that run are encoded below rather than left to be
//! rediscovered. A reasoning model puts its thinking in a **separate**
//! `reasoning` field and leaves `content` empty until it has finished, so a
//! budget that runs out mid-thought yields a perfectly valid response carrying
//! nothing at all — `finish_reason: "length"`, `content: ""`. And `reasoning_effort`
//! is not a field a backend can be relied on to ignore: Ollama 0.32 dropped
//! what it did not recognise, and 0.33 reads that one, answering `400
//! "<model>" does not support thinking` for every model with no thinking mode
//! — which is most of the ones anyone runs locally. It is still sent, and
//! dropped on exactly that refusal; see `complete` below.

use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

use crate::LlmError;
use crate::config::LlmConfig;
use crate::http;
use crate::provider::{Completion, CompletionOutput, Provider};

/// A client for any API in the OpenAI chat-completions shape.
pub struct OpenAiCompatible {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<secrecy::SecretString>,
    model: String,
    effort: crate::config::Effort,
    /// The highest effort this backend has a word for.
    ///
    /// Every level below it is sent unchanged; anything above is sent as this.
    /// Only backends that are known to refuse the top levels set it, and they
    /// set it because the refusal is indistinguishable from the other 400s —
    /// see `refuses_thinking` for the case where a backend does say what it
    /// means, and `GOOGLE_MAX_EFFORT` for the case where it does not.
    effort_ceiling: crate::config::Effort,
    max_retries: u32,
    /// What to call this backend in a log line. Not cosmetic: "the model
    /// refused" reads very differently depending on whether it is a hosted
    /// service or a process on the same machine.
    name: &'static str,
}

impl std::fmt::Debug for OpenAiCompatible {
    /// Hand-written so no future `{:?}` can print the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatible")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatible {
    /// Build a client from configuration.
    ///
    /// The key is optional, which is the difference that matters. A model
    /// running on this machine has no credential to present, and demanding one
    /// would make the local path — the one that costs nothing and sends
    /// nobody's session transcript anywhere — impossible to configure.
    pub fn new(config: &LlmConfig, name: &'static str) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder().timeout(config.timeout).build()?;

        Ok(Self {
            http,
            endpoint: format!("{}/chat/completions", config.base_url),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            effort: config.effort,
            effort_ceiling: crate::config::Effort::Max,
            max_retries: config.max_retries,
            name,
        })
    }

    /// Cap the effort this backend is asked for.
    ///
    /// Additive on purpose: a backend that understands the whole vocabulary
    /// says nothing, so adding one that does not cannot change what the others
    /// send.
    pub fn with_effort_ceiling(mut self, ceiling: crate::config::Effort) -> Self {
        self.effort_ceiling = ceiling;
        self
    }

    /// One attempt, no retry logic.
    async fn attempt(&self, body: &Value) -> Result<CompletionOutput, LlmError> {
        let mut request = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json");

        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key.expose_secret());
        }

        let response = request.json(body).send().await?;
        let status = response.status();
        let retry_after = http::retry_after(response.headers());
        let text = response.text().await?;

        if !status.is_success() {
            return Err(http::api_error(status.as_u16(), &text, retry_after));
        }

        let payload: Value = serde_json::from_str(&text)
            .map_err(|error| LlmError::Malformed(format!("response was not JSON: {error}")))?;

        parse_response(&payload)
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError> {
        let mut body = build_body(&self.model, self.effort.min(self.effort_ceiling), request);
        let mut asked_to_think = true;

        let mut attempt = 0;
        loop {
            match self.attempt(&body).await {
                Ok(output) => return Ok(output),
                // Not a retry, and deliberately not counted as one: the same
                // request again would be refused the same way. The effort was
                // never the point of the call, so it goes and the call stands
                // — the alternative is a session losing its page over a
                // setting that means nothing to the model answering it.
                Err(error) if asked_to_think && refuses_thinking(&error) => {
                    tracing::debug!(
                        provider = self.name,
                        model = %self.model,
                        "model has no thinking to configure; asking again without reasoning_effort"
                    );
                    if let Some(object) = body.as_object_mut() {
                        object.remove("reasoning_effort");
                    }
                    asked_to_think = false;
                }
                Err(error) if attempt < self.max_retries && error.is_retryable() => {
                    let delay = http::retry_delay(&error, attempt);
                    tracing::warn!(
                        provider = self.name,
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

/// Whether a refusal is the backend saying this model has no thinking to
/// configure, rather than anything about the request a caller could fix.
///
/// Matched on the message because the message is where it is said: the status
/// is a plain 400 and the type a plain `invalid_request_error`, and both of
/// those cover a dozen other faults that must not quietly lose a setting.
fn refuses_thinking(error: &LlmError) -> bool {
    matches!(
        error,
        LlmError::Api { status: 400, message, .. } if message.contains("does not support thinking")
    )
}

/// Assemble the request body.
///
/// `strict` is set because the point of naming a schema is not having to parse
/// whatever prose came back. `additionalProperties` is left to the schema the
/// caller supplied: OpenAI's strict mode requires it to be `false`, and a
/// caller whose schema says otherwise has said something deliberate that this
/// is not the place to overrule.
///
/// `reasoning_effort` is sent to every backend, because the backends that read
/// it are the ones the setting exists for. A backend that refuses it says so in
/// a 400 that names thinking, and `complete` sends the request again without
/// the field — so the setting keeps meaning something on OpenAI without costing
/// every non-thinking local model its page.
///
/// The effort arriving here has already passed the caller's ceiling, for the
/// backends whose refusal names nothing at all.
fn build_body(model: &str, effort: crate::config::Effort, request: &Completion) -> Value {
    json!({
        "model": model,
        "max_tokens": request.max_output_tokens,
        "stream": false,
        "reasoning_effort": effort.as_str(),
        "messages": [
            { "role": "system", "content": request.system },
            { "role": "user", "content": request.user },
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "anamnesis_digest",
                "strict": true,
                "schema": request.schema,
            },
        },
    })
}

/// Turn a successful response into an answer, or explain why it is not one.
fn parse_response(payload: &Value) -> Result<CompletionOutput, LlmError> {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| LlmError::Malformed("response carried no choices".to_owned()))?;

    let message = choice
        .get("message")
        .ok_or_else(|| LlmError::Malformed("choice carried no message".to_owned()))?;

    // Structured-output refusals come back as a field rather than an error
    // status, so a caller that only checked the HTTP code would read the
    // refusal as an empty answer.
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str)
        && !refusal.trim().is_empty()
    {
        return Err(LlmError::Refused {
            category: Some(refusal.trim().to_owned()),
        });
    }

    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();

    if finish == "length" {
        // Distinguished from an ordinary truncation because the fix is
        // different, and because a reasoning model reaches it while looking
        // completely healthy: HTTP 200, a full `reasoning` field, and not one
        // character of the answer.
        let thinking = message
            .get("reasoning")
            .and_then(Value::as_str)
            .is_some_and(|reasoning| !reasoning.trim().is_empty());
        return Err(LlmError::Malformed(if content.is_empty() && thinking {
            "the model spent its whole output budget thinking and answered nothing; \
             raise ANAMNESIS_LLM_MAX_OUTPUT_TOKENS or choose a model that reasons less"
                .to_owned()
        } else {
            "reply hit the output limit and is incomplete; raise ANAMNESIS_LLM_MAX_OUTPUT_TOKENS"
                .to_owned()
        }));
    }

    if content.is_empty() {
        return Err(LlmError::Malformed(
            "response carried no content".to_owned(),
        ));
    }

    let json: Value = serde_json::from_str(content).map_err(|error| {
        LlmError::Malformed(format!("reply was not the JSON we asked for: {error}"))
    })?;

    Ok(CompletionOutput {
        json,
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        input_tokens: http::usage(payload, "prompt_tokens"),
        output_tokens: http::usage(payload, "completion_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn request() -> Completion {
        Completion {
            system: "you summarise sessions".to_owned(),
            user: "a session happened".to_owned(),
            schema: json!({"type": "object", "properties": {"title": {"type": "string"}}}),
            max_output_tokens: 2_000,
        }
    }

    #[test]
    fn the_body_asks_for_json_in_our_schema() {
        let body = build_body("gpt-5", Effort::High, &request());

        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["max_tokens"], 2_000);
        assert_eq!(body["stream"], false);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["properties"]["title"]["type"],
            "string"
        );
    }

    /// Ollama 0.33 refuses `reasoning_effort` outright for a model with no
    /// thinking mode, and the refusal is a plain 400 of the same type and
    /// status as a malformed schema or an unknown model. Only the message
    /// tells them apart, so only the message may be allowed to.
    #[test]
    fn a_model_with_no_thinking_mode_is_told_apart_from_other_refusals() {
        let no_thinking = LlmError::Api {
            status: 400,
            kind: "invalid_request_error".to_owned(),
            message: "\"qwen2.5:7b-instruct\" does not support thinking".to_owned(),
        };
        assert!(refuses_thinking(&no_thinking));

        let other_400 = LlmError::Api {
            status: 400,
            kind: "invalid_request_error".to_owned(),
            message: "model \"gpt-5\" not found".to_owned(),
        };
        assert!(!refuses_thinking(&other_400));

        // The same sentence from a failing server is a fault to retry, not a
        // setting to drop.
        let server_fault = LlmError::Api {
            status: 500,
            kind: "server_error".to_owned(),
            message: "\"qwen\" does not support thinking".to_owned(),
        };
        assert!(!refuses_thinking(&server_fault));
    }

    /// What `complete` sends on the second try. Everything the page depends on
    /// has to survive dropping the effort — the schema above all, since a
    /// request that lost it would come back as prose and be blamed on the
    /// model.
    #[test]
    fn dropping_the_effort_leaves_the_rest_of_the_request_intact() {
        let mut body = build_body("qwen2.5:7b-instruct", Effort::High, &request());
        body.as_object_mut()
            .expect("object")
            .remove("reasoning_effort");

        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["model"], "qwen2.5:7b-instruct");
        assert_eq!(body["max_tokens"], 2_000);
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["messages"][0]["role"], "system");
    }

    /// The ceiling is the whole of Google's support: it takes `high` but has
    /// never heard of `xhigh` or `max`, and says so in a 400 that names
    /// neither thinking nor the field. Nothing downstream could tell that
    /// refusal from an unknown model, so the clamp has to happen before the
    /// request leaves.
    #[test]
    fn an_effort_above_what_a_backend_knows_is_sent_as_the_most_it_knows() {
        for asked in [Effort::XHigh, Effort::Max] {
            assert_eq!(
                asked.min(Effort::High).as_str(),
                "high",
                "{asked:?} should arrive as high"
            );
        }
    }

    /// And the clamp only clamps: a backend with a ceiling still hears every
    /// level below it, or the setting would mean one thing on OpenAI and
    /// nothing at all here.
    #[test]
    fn a_ceiling_leaves_the_levels_under_it_alone() {
        for asked in [Effort::Low, Effort::Medium, Effort::High] {
            assert_eq!(asked.min(Effort::High), asked, "{asked:?} should pass");
        }
    }

    /// The default is no ceiling at all, so adding one backend that needs it
    /// cannot quietly lower what every other backend is asked for.
    #[test]
    fn a_backend_that_sets_no_ceiling_is_asked_for_everything() {
        let config = LlmConfig {
            provider: crate::config::ProviderKind::OpenAi,
            effort: Effort::Max,
            ..LlmConfig::default()
        };
        let client = OpenAiCompatible::new(&config, "openai").expect("builds");

        assert_eq!(client.effort.min(client.effort_ceiling), Effort::Max);
    }

    /// The system prompt is a separate message here, unlike Anthropic's
    /// top-level field. Sending it as one user turn would work and would also
    /// quietly give up prompt caching on every backend that keys on the system
    /// message.
    #[test]
    fn the_system_prompt_is_its_own_message() {
        let body = build_body("gpt-5", Effort::High, &request());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "you summarise sessions");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    /// Google's compatible surface authenticates with a bearer token and
    /// refuses a request that also carries `x-goog-api-key` or a `?key=`
    /// parameter — `400 Multiple authentication credentials received`, which
    /// reads like a bad key rather than like two of them. This client has only
    /// ever sent one, and this is the test that keeps it that way.
    #[tokio::test]
    async fn a_request_carries_exactly_one_credential() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");

        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let (mut socket, _) = listener.accept().expect("accept");
            let mut buffer = [0_u8; 8192];
            let read = socket.read(&mut buffer).expect("read");
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();

            let answer = json!({
                "model": "gemini-2.5-flash",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "{\"title\": \"done\"}"},
                    "finish_reason": "stop",
                }],
            })
            .to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                )
                .expect("write");
            request
        });

        // Built the way configuration builds it, pointed at the socket. A
        // client assembled by hand here would prove nothing about what the
        // google provider actually sends.
        let vars: Vec<(&str, String)> = vec![
            ("ANAMNESIS_LLM_PROVIDER", "google".to_owned()),
            ("GEMINI_API_KEY", "AQ.test-key-not-a-real-one".to_owned()),
            ("ANAMNESIS_LLM_BASE_URL", format!("http://{address}/v1beta")),
            ("ANAMNESIS_LLM_EFFORT", "max".to_owned()),
        ];
        let provider = LlmConfig::from_vars(|key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        })
        .expect("configures")
        .build()
        .expect("builds")
        .expect("google is a provider");

        let output = provider.complete(&request()).await.expect("completes");
        assert_eq!(output.json["title"], "done");

        let sent = server.join().expect("server thread");
        let lowered = sent.to_ascii_lowercase();

        assert!(
            sent.starts_with("POST /v1beta/chat/completions"),
            "wrong path: {sent}"
        );
        assert!(
            lowered.contains("authorization: bearer aq.test-key-not-a-real-one"),
            "the key was not presented: {sent}"
        );
        assert!(
            !lowered.contains("x-goog-api-key"),
            "a second credential rode along: {sent}"
        );
        assert!(
            !sent.contains("key="),
            "the key leaked into the URL: {sent}"
        );
        // And the ceiling held on the way out, which is the other half of
        // being able to talk to this backend at all.
        assert!(
            sent.contains("\"reasoning_effort\":\"high\""),
            "effort was not clamped: {sent}"
        );
    }

    fn reply(message: Value, finish: &str) -> Value {
        json!({
            "model": "gpt-5",
            "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 22 },
        })
    }

    #[test]
    fn a_good_reply_is_parsed_with_its_usage() {
        let output = parse_response(&reply(
            json!({"role": "assistant", "content": "{\"title\": \"done\"}"}),
            "stop",
        ))
        .expect("parses");

        assert_eq!(output.json["title"], "done");
        assert_eq!(output.model, "gpt-5");
        assert_eq!(output.input_tokens, 11);
        assert_eq!(output.output_tokens, 22);
    }

    /// The finding this provider was written around. A reasoning model on a
    /// small budget returns HTTP 200, a full `reasoning` field, and an empty
    /// answer — measured against Ollama, which produced exactly this at 400
    /// tokens and a complete answer at 2500.
    #[test]
    fn a_model_that_thought_until_the_budget_ran_out_says_so() {
        let error = parse_response(&reply(
            json!({
                "role": "assistant",
                "content": "",
                "reasoning": "First, the user asked for a summary. I should…",
            }),
            "length",
        ))
        .expect_err("should not parse");

        let message = error.to_string();
        assert!(message.contains("thinking"), "{message}");
        assert!(
            message.contains("ANAMNESIS_LLM_MAX_OUTPUT_TOKENS"),
            "{message}"
        );
    }

    /// Truncation without thinking is a different sentence, because it points
    /// at the same setting for a different reason.
    #[test]
    fn a_truncated_answer_is_reported_as_truncation() {
        let error = parse_response(&reply(
            json!({"role": "assistant", "content": "{\"title\": \"do"}),
            "length",
        ))
        .expect_err("should not parse");

        let message = error.to_string();
        assert!(message.contains("incomplete"), "{message}");
        assert!(!message.contains("thinking"), "{message}");
    }

    /// A refusal arrives as a field on a 200, so checking the status alone
    /// would read it as an empty answer and blame the schema.
    #[test]
    fn a_refusal_is_a_refusal_rather_than_an_empty_answer() {
        let error = parse_response(&reply(
            json!({"role": "assistant", "content": null, "refusal": "I cannot help with that"}),
            "stop",
        ))
        .expect_err("should not parse");

        match error {
            LlmError::Refused { category } => {
                assert_eq!(category.as_deref(), Some("I cannot help with that"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn prose_where_json_was_asked_for_is_malformed_rather_than_accepted() {
        let error = parse_response(&reply(
            json!({"role": "assistant", "content": "Sure! Here is your summary."}),
            "stop",
        ))
        .expect_err("should not parse");
        assert!(error.to_string().contains("not the JSON"), "{error}");
    }

    #[test]
    fn an_empty_reply_is_not_mistaken_for_an_answer() {
        let error = parse_response(&reply(json!({"role": "assistant", "content": ""}), "stop"))
            .expect_err("should not parse");
        assert!(error.to_string().contains("no content"), "{error}");
    }

    #[test]
    fn a_response_without_choices_is_refused() {
        let error = parse_response(&json!({"model": "gpt-5"})).expect_err("should not parse");
        assert!(error.to_string().contains("no choices"), "{error}");
    }
}
