//! The thinking-effort fallback, against a real Ollama.
//!
//! The unit tests beside `openai.rs` cover the two halves separately — that a
//! refusal naming thinking is told apart from every other 400, and that a body
//! stripped of `reasoning_effort` still carries its schema. Neither runs the
//! loop in `complete` that joins them, and that loop is the whole fix: a
//! version of it that dropped the field and then returned the original error
//! anyway would pass both unit tests and leave every local session without a
//! page.
//!
//! Ignored because it needs a server. Run it with one:
//!
//! ```text
//! ollama serve
//! ollama pull qwen2.5:7b-instruct
//! cargo test -p anamnesis-llm --test live_ollama -- --ignored --nocapture
//! ```

use anamnesis_llm::{Completion, LlmConfig, Provider};
use serde_json::json;

/// A model with no thinking mode, which is the case under test. Overridable
/// because the point is the *class* of model, not this one — anything without
/// a thinking capability exercises the same path.
fn model() -> String {
    std::env::var("ANAMNESIS_TEST_OLLAMA_MODEL")
        .unwrap_or_else(|_| "qwen2.5:7b-instruct".to_owned())
}

/// The provider as configuration actually builds it, rather than a client
/// assembled by hand here. A fallback that worked only when the test wired it
/// up itself would not be worth running.
fn ollama(model: &str) -> std::sync::Arc<dyn Provider> {
    let vars: Vec<(&str, String)> = vec![
        ("ANAMNESIS_LLM_PROVIDER", "ollama".to_owned()),
        ("ANAMNESIS_LLM_MODEL", model.to_owned()),
    ];

    LlmConfig::from_vars(|key| {
        vars.iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.clone())
    })
    .expect("ollama needs no key")
    .build()
    .expect("provider builds")
    .expect("ollama is a provider")
}

/// Shaped like a consolidation: a schema with named fields, because the
/// failure this guards against is the reply coming back as prose.
fn request() -> Completion {
    Completion {
        system: "You summarise coding sessions. Answer only with JSON in the given schema."
            .to_owned(),
        user: "The session renamed a project and updated its migration notes.".to_owned(),
        schema: json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
            "additionalProperties": false
        }),
        max_output_tokens: 200,
    }
}

/// Confirm the backend still refuses `reasoning_effort` for this model.
///
/// Without this the test above would keep passing for the wrong reason the day
/// Ollama starts accepting the field: a green run would mean "nothing to
/// recover from" while reading as "recovery works". Mirrors what `build_body`
/// sends, since that function is private to the crate.
async fn still_refuses_thinking(model: &str) -> bool {
    let response = reqwest::Client::new()
        .post("http://127.0.0.1:11434/v1/chat/completions")
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 16,
            "reasoning_effort": "high"
        }))
        .send()
        .await
        .expect("ollama is running");

    let status = response.status();
    let body = response.text().await.expect("a body");

    status == 400 && body.contains("does not support thinking")
}

/// The end-to-end claim: effort is set, the model has no thinking to
/// configure, and a page comes back regardless.
#[tokio::test]
#[ignore = "needs a local Ollama with a non-thinking model pulled"]
async fn a_model_with_no_thinking_mode_still_answers_with_effort_set() {
    let model = model();

    assert!(
        still_refuses_thinking(&model).await,
        "{model} no longer refuses reasoning_effort, so this test is no longer          exercising the fallback — pick a model without a thinking mode, or          retire the fallback"
    );

    let provider = ollama(&model);

    let output = provider
        .complete(&request())
        .await
        .expect("a model with no thinking mode still answers");

    assert_eq!(output.model, model);
    assert!(
        output.json["title"].is_string(),
        "reply lost its schema on the second attempt: {}",
        output.json
    );
    assert!(
        output.output_tokens > 0,
        "no tokens were billed for a reply"
    );

    println!("{model} answered: {}", output.json);
}

/// The other half of the same claim, and the one a message-matched fallback
/// could get wrong: a model that does not exist must still fail. Dropping
/// `reasoning_effort` and retrying forever on any 400 would turn every typo in
/// a model name into a silent hang or a second identical refusal reported as
/// something else.
#[tokio::test]
#[ignore = "needs a local Ollama"]
async fn a_model_that_does_not_exist_is_still_an_error() {
    let provider = ollama("no-such-model:never-pulled");

    let error = provider
        .complete(&request())
        .await
        .expect_err("an unknown model is not answerable");

    let message = error.to_string();
    assert!(
        !message.contains("does not support thinking"),
        "an unknown model was reported as a thinking problem: {message}"
    );

    println!("unknown model failed as: {message}");
}
