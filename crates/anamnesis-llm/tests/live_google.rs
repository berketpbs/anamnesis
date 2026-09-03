//! Google AI Studio, against the real endpoint.
//!
//! The unit tests beside `openai.rs` and `config.rs` cover the pieces: that
//! the provider resolves to Google's address, that the request leaves with one
//! credential, that an effort above `high` is clamped. What none of them can
//! check is whether Google agrees — whether the compatible surface honours a
//! strict `json_schema`, whether it takes `reasoning_effort` at all, and
//! whether an `AQ.` key authenticates as a bearer token. Those are claims
//! about somebody else's service, and the only honest way to hold them is to
//! ask it.
//!
//! Ignored because it needs a key and spends money. Run it with one, without
//! putting the key on a command line:
//!
//! ```text
//! powershell -NoProfile -File "$env:APPDATA\anamnesis\bin\with-llm-env.ps1" -- \
//!     cargo test -p anamnesis-llm --test live_google -- --ignored --nocapture
//! ```
//!
//! Or, anywhere the key is already exported:
//!
//! ```text
//! GEMINI_API_KEY=AQ.... cargo test -p anamnesis-llm --test live_google -- --ignored --nocapture
//! ```

use anamnesis_llm::{Completion, LlmConfig, Provider};
use serde_json::json;

/// The model under test. Overridable because the claim is about the *surface*,
/// not about one model on it: any Gemini the account can reach exercises the
/// same schema, the same auth, and the same effort vocabulary.
fn model() -> String {
    std::env::var("ANAMNESIS_TEST_GOOGLE_MODEL").unwrap_or_else(|_| "gemini-3.6-flash".to_owned())
}

/// The key, from the environment this test was given.
///
/// Fails loudly rather than skipping: a live test that quietly passes when it
/// never ran is worse than no live test, because the green tick is the thing
/// people read.
fn key() -> String {
    for name in ["ANAMNESIS_LLM_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"] {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            return value;
        }
    }

    panic!(
        "no key in the environment; run this under with-llm-env.ps1, or export \
         GEMINI_API_KEY for the shell that runs it"
    )
}

/// The provider as configuration builds it, effort included. Assembling a
/// client by hand here would test this file rather than the crate.
fn google(model: &str, effort: &str) -> std::sync::Arc<dyn Provider> {
    let vars: Vec<(&str, String)> = vec![
        ("ANAMNESIS_LLM_PROVIDER", "google".to_owned()),
        ("ANAMNESIS_LLM_API_KEY", key()),
        ("ANAMNESIS_LLM_MODEL", model.to_owned()),
        ("ANAMNESIS_LLM_EFFORT", effort.to_owned()),
    ];

    LlmConfig::from_vars(|name| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    })
    .expect("google configures")
    .build()
    .expect("provider builds")
    .expect("google is a provider")
}

/// Shaped like a consolidation: several named fields, one of them a list, and
/// `additionalProperties: false` — the shape `digest_from_json` reads. A
/// backend that honours a schema loosely tends to honour it exactly here and
/// loosely on the second field.
fn request() -> Completion {
    Completion {
        system: "You summarise coding sessions for a memory system. \
                 Answer only with JSON in the given schema."
            .to_owned(),
        user: "The session added Google AI Studio as a provider, clamped the \
               reasoning effort to what that backend understands, and proved \
               with a socket test that only one credential is ever sent."
            .to_owned(),
        schema: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "summary": {"type": "string"},
                "entities": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["title", "summary", "entities"],
            "additionalProperties": false
        }),
        max_output_tokens: 2_000,
    }
}

/// The claim the provider exists for: a real key, a strict schema, and a reply
/// with exactly the fields consolidation will read out of it.
#[tokio::test]
#[ignore = "needs a Google AI Studio key"]
async fn google_answers_a_consolidation_in_the_schema_it_was_given() {
    let model = model();
    let provider = google(&model, "high");

    let output = provider
        .complete(&request())
        .await
        .expect("google answers a schema-constrained request");

    assert!(
        output.json["title"].is_string(),
        "reply lost its schema: {}",
        output.json
    );
    assert!(
        output.json["summary"].is_string(),
        "reply lost its schema: {}",
        output.json
    );
    assert!(
        output.json["entities"].is_array(),
        "reply lost its schema: {}",
        output.json
    );
    assert!(
        output.output_tokens > 0,
        "no tokens were billed for a reply"
    );

    println!("{} answered: {}", output.model, output.json);
}

/// The other half, and the reason the ceiling exists. `max` is a level Google
/// has no word for; sent as written it is refused with a 400 that names
/// neither thinking nor the field, and the session loses its page. Clamped, it
/// is an ordinary request — so a green run here is the ceiling working, and a
/// 400 mentioning `reasoning_effort` is the ceiling gone.
#[tokio::test]
#[ignore = "needs a Google AI Studio key"]
async fn an_effort_google_has_no_word_for_still_gets_a_page() {
    let model = model();
    let provider = google(&model, "max");

    let output = provider.complete(&request()).await.unwrap_or_else(|error| {
        panic!("effort above google's vocabulary was not clamped: {error}")
    });

    assert!(
        output.json["title"].is_string(),
        "reply lost its schema: {}",
        output.json
    );

    println!(
        "{} answered at clamped effort: {}",
        output.model, output.json
    );
}
