//! `anamnesis key check`: ask the configured model one small question, and
//! say what came back.
//!
//! A key is set with `anamnesis key set` and read by the server when it
//! starts. Between the two there was nothing to run. The way to find out
//! whether a new key worked was to restart the server, finish a session, and
//! read `status` — and on 2026-09-14 a key that had stopped working was found
//! a day later, from 820 refusals in a log.
//!
//! This reads the settings the way every command does — environment, then
//! `settings.env`, then the credential store — builds each model the server
//! would build, and sends each one request. Each link of a fallback chain is
//! asked on its own, because a chain hides the link that did not answer, and
//! the point is to know about every key before a server depends on it.
//!
//! It proves less than it looks like it proves, and says so. An answer to a
//! one-line question shows the key is accepted and the model is reachable; it
//! does not show the model will serve a real consolidation. On 2026-09-07 two
//! Gemini models answered a ping and refused every real request with `503`.

use anamnesis_llm::{Completion, LlmConfig, LlmError, ProviderKind};
use serde_json::json;

/// What one model made of the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It answered. The key works.
    Answered { model: String },
    /// The API accepted the request and the answer was not what was asked for
    /// — cut short, not JSON, declined. The key works; this check needs
    /// nothing more from the reply.
    AcceptedWithoutAnswer { why: String },
    /// Authenticated, and today's quota for this model is spent.
    SpentForTheDay { message: String },
    /// Authenticated, and rate limited for now.
    RateLimited { message: String },
    /// The key was refused.
    KeyRefused { status: u16, message: String },
    /// The key may be fine; this model is not available to it.
    NoSuchModel { status: u16, message: String },
    /// The service failed or is overloaded. Says nothing about the key.
    Unavailable { status: u16, message: String },
    /// Nothing answered.
    Unreachable { message: String },
    /// Something else, named as it came.
    Other { message: String },
}

impl Verdict {
    /// Read one attempt.
    pub fn of(result: &Result<String, LlmError>) -> Self {
        let error = match result {
            Ok(model) => {
                return Self::Answered {
                    model: model.clone(),
                };
            }
            Err(error) => error,
        };
        match error {
            LlmError::Truncated(_) => Self::AcceptedWithoutAnswer {
                why: "the reply did not fit its budget".to_owned(),
            },
            LlmError::Malformed(why) => Self::AcceptedWithoutAnswer {
                why: format!("the reply was not the JSON asked for ({why})"),
            },
            LlmError::Refused { .. } => Self::AcceptedWithoutAnswer {
                why: "the model declined the question".to_owned(),
            },
            LlmError::Api { status: 429, .. } if error.is_spent_for_the_day() => {
                Self::SpentForTheDay {
                    message: sentence(error),
                }
            }
            LlmError::Api { status: 429, .. } => Self::RateLimited {
                message: sentence(error),
            },
            LlmError::Api {
                status: status @ (401 | 403),
                ..
            } => Self::KeyRefused {
                status: *status,
                message: sentence(error),
            },
            // Google answers a bad key with a plain 400, and says so only in
            // the sentence: "Please pass a valid API key", "API key not
            // valid", "API key expired".
            LlmError::Api {
                status: 400,
                message,
                ..
            } if message.to_ascii_lowercase().contains("api key") => Self::KeyRefused {
                status: 400,
                message: sentence(error),
            },
            LlmError::Api { status: 404, .. } => Self::NoSuchModel {
                status: 404,
                message: sentence(error),
            },
            LlmError::Api { status, .. } if *status >= 500 => Self::Unavailable {
                status: *status,
                message: sentence(error),
            },
            LlmError::Transport(_) => Self::Unreachable {
                message: error.to_string(),
            },
            LlmError::Api { .. } | LlmError::Config(_) => Self::Other {
                message: error.to_string(),
            },
        }
    }

    /// Whether this says the key works.
    pub fn key_works(&self) -> bool {
        matches!(
            self,
            Self::Answered { .. }
                | Self::AcceptedWithoutAnswer { .. }
                | Self::SpentForTheDay { .. }
                | Self::RateLimited { .. }
        )
    }

    /// One line for a person.
    pub fn describe(&self) -> String {
        match self {
            Self::Answered { model } => format!("✅ answered, as {model}: the key works"),
            Self::AcceptedWithoutAnswer { why } => {
                format!("✅ the key was accepted ({why}; this check needs nothing more)")
            }
            Self::SpentForTheDay { message } => format!(
                "✅ the key was accepted, and today's quota for this model is spent: {message}"
            ),
            Self::RateLimited { message } => {
                format!("✅ the key was accepted, and requests are limited right now: {message}")
            }
            Self::KeyRefused { status, message } => {
                format!("❌ the key was refused ({status}): {message}")
            }
            Self::NoSuchModel { status, message } => format!(
                "❌ this model is not available ({status}): {message} — check ANAMNESIS_LLM_MODEL"
            ),
            Self::Unavailable { status, message } => format!(
                "⚠️  the service did not answer ({status}): {message} — this says nothing about the key; try again"
            ),
            Self::Unreachable { message } => format!("❌ nothing answered: {message}"),
            Self::Other { message } => format!("❌ {message}"),
        }
    }
}

/// The provider's own sentence, without the wait the retry loop reads.
fn sentence(error: &LlmError) -> String {
    match error {
        LlmError::Api { message, .. } => message
            .rsplit_once(" (retry after ")
            .map_or(message.as_str(), |(head, _)| head)
            .to_owned(),
        other => other.to_string(),
    }
}

/// The question. Small, so it costs next to nothing, and schema-bound, so it
/// goes through the same path a consolidation does.
fn question(max_output_tokens: u32) -> Completion {
    Completion {
        system: "You are checking that a model is reachable. Answer only with JSON in the given \
                 schema."
            .to_owned(),
        user: "Set ok to true.".to_owned(),
        schema: json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "additionalProperties": false
        }),
        max_output_tokens,
    }
}

/// Every model `config` names, first the configured one and then each
/// fallback, each on its own and asked once.
pub fn links(config: &LlmConfig) -> Vec<LlmConfig> {
    std::iter::once(config.without_fallbacks())
        .chain(config.fallbacks.iter().cloned())
        .map(|link| LlmConfig {
            // One request per model. A retry spends quota finding out what
            // the first answer already said, and a person is waiting.
            max_retries: 0,
            ..link
        })
        .collect()
}

/// Ask one model.
pub async fn ask(link: &LlmConfig) -> Verdict {
    let provider = match link.build() {
        Ok(Some(provider)) => provider,
        Ok(None) => {
            return Verdict::Other {
                message: "no model is configured".to_owned(),
            };
        }
        Err(error) => {
            return Verdict::Other {
                message: error.to_string(),
            };
        }
    };
    let result = provider
        .complete(&question(link.max_output_tokens))
        .await
        .map(|output| output.model);
    Verdict::of(&result)
}

/// The variable a link's key was read from, following the choice `LlmConfig`
/// makes: the configured provider takes `ANAMNESIS_LLM_API_KEY` when a
/// provider is named and that key is set, a fallback to the same provider
/// carries the configured one's key, and any other provider reads only its
/// own variables.
fn key_variable(
    primary: &LlmConfig,
    link: &LlmConfig,
    var: impl Fn(&str) -> Option<String>,
) -> Option<&'static str> {
    link.api_key.as_ref()?;
    let set = |name: &str| var(name).is_some_and(|value| !value.trim().is_empty());
    if link.provider == primary.provider
        && set("ANAMNESIS_LLM_PROVIDER")
        && set("ANAMNESIS_LLM_API_KEY")
    {
        return Some("ANAMNESIS_LLM_API_KEY");
    }
    match link.provider {
        ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
        ProviderKind::OpenAi => Some("OPENAI_API_KEY"),
        ProviderKind::Google if set("GEMINI_API_KEY") => Some("GEMINI_API_KEY"),
        ProviderKind::Google => Some("GOOGLE_API_KEY"),
        ProviderKind::Ollama | ProviderKind::None => None,
    }
}

/// Where a variable's value came from, in the order `settings::var` reads.
fn origin(name: &str) -> &'static str {
    if std::env::var(name).is_ok_and(|value| !value.trim().is_empty()) {
        "this shell's environment"
    } else {
        "the credential store"
    }
}

/// `anamnesis key check`.
pub fn cmd_key_check() -> anyhow::Result<()> {
    println!("🔑 Checking the model key");
    println!();

    let config = match LlmConfig::from_vars(crate::settings::var) {
        Ok(config) => config,
        Err(error) => anyhow::bail!("the model settings do not load: {error}"),
    };
    if config.provider == ProviderKind::None {
        match config.ignored_key {
            Some(key) => anyhow::bail!(
                "{key} is set, and ANAMNESIS_LLM_PROVIDER does not name the provider it is for, \
                 so nothing would use it. Name one in settings.env, e.g. ANAMNESIS_LLM_PROVIDER=google"
            ),
            None => anyhow::bail!(
                "no model is configured: set ANAMNESIS_LLM_PROVIDER (and ANAMNESIS_LLM_MODEL) in \
                 settings.env, and the key with `anamnesis key set`"
            ),
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let mut refused = 0usize;
    for (index, link) in links(&config).iter().enumerate() {
        let role = if index == 0 { "model" } else { "fallback" };
        println!("  {role:<8} {} ({})", link.model, link.base_url);
        if let Some(variable) = key_variable(&config, link, crate::settings::var) {
            println!("  key      {variable}, from {}", origin(variable));
        }
        let verdict = runtime.block_on(ask(link));
        println!("  {}", verdict.describe());
        println!();
        if !verdict.key_works() {
            refused += 1;
        }
    }

    println!("  This asked a one-line question. A model that answers it can still refuse a");
    println!("  real consolidation (2026-09-07: two models answered this and gave 503 to");
    println!("  every session), so read `anamnesis status` after the next session ends.");
    println!("  A running server read its key when it started: restart it to use a new one.");

    if refused > 0 {
        anyhow::bail!(
            "{refused} of {} model(s) could not be shown to work",
            links(&config).len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, message: &str) -> Result<String, LlmError> {
        Err(LlmError::Api {
            status,
            kind: "unknown".to_owned(),
            message: message.to_owned(),
        })
    }

    /// The refusals this machine has actually had, and what each has to be
    /// called: the day the key stopped, the day the quota ran out, and the
    /// afternoon the model was overloaded.
    #[test]
    fn each_answer_is_named_for_what_it_says_about_the_key() {
        assert_eq!(
            Verdict::of(&api(400, "Please pass a valid API key")),
            Verdict::KeyRefused {
                status: 400,
                message: "Please pass a valid API key".to_owned()
            }
        );
        assert!(matches!(
            Verdict::of(&api(403, "Your API key was reported as leaked.")),
            Verdict::KeyRefused { status: 403, .. }
        ));
        assert!(matches!(
            Verdict::of(&api(401, "invalid x-api-key")),
            Verdict::KeyRefused { status: 401, .. }
        ));

        let spent = Verdict::of(&api(
            429,
            "You exceeded your current quota. [GenerateRequestsPerDayPerProjectPerModel-FreeTier] (retry after 31s)",
        ));
        assert_eq!(
            spent,
            Verdict::SpentForTheDay {
                message: "You exceeded your current quota. [GenerateRequestsPerDayPerProjectPerModel-FreeTier]"
                    .to_owned()
            }
        );
        assert!(
            spent.key_works(),
            "a quota is counted against an accepted key"
        );

        assert!(matches!(
            Verdict::of(&api(429, "slow down")),
            Verdict::RateLimited { .. }
        ));

        let overloaded = Verdict::of(&api(503, "The model is overloaded."));
        assert!(matches!(
            overloaded,
            Verdict::Unavailable { status: 503, .. }
        ));
        assert!(!overloaded.key_works(), "a 503 proves nothing either way");
        assert!(overloaded.describe().contains("says nothing about the key"));

        assert!(matches!(
            Verdict::of(&api(404, "models/gemini-2.5-flash is no longer available")),
            Verdict::NoSuchModel { status: 404, .. }
        ));
        assert!(
            matches!(
                Verdict::of(&api(400, "Invalid JSON payload received.")),
                Verdict::Other { .. }
            ),
            "a 400 that does not mention the key is not called a refused key"
        );
    }

    /// The API accepted the request, and that is all this asks.
    #[test]
    fn an_answer_that_is_not_the_answer_still_shows_the_key_works() {
        for error in [
            LlmError::Truncated("did not fit".to_owned()),
            LlmError::Malformed("not json".to_owned()),
            LlmError::Refused { category: None },
        ] {
            let verdict = Verdict::of(&Err(error));
            assert!(verdict.key_works(), "{verdict:?}");
        }
        assert!(Verdict::of(&Ok("gemini-3.5-flash".to_owned())).key_works());
    }

    /// Every link asked on its own and once, because a chain hides the link
    /// that did not answer and a retry spends quota on the same answer.
    #[test]
    fn every_link_is_asked_on_its_own_once() {
        let pairs = [
            ("ANAMNESIS_LLM_PROVIDER", "google"),
            ("ANAMNESIS_LLM_API_KEY", "AQ.test-key"),
            ("ANAMNESIS_LLM_MODEL", "gemini-3.5-flash"),
            (
                "ANAMNESIS_LLM_FALLBACK_PROVIDERS",
                "google:gemini-3.6-flash",
            ),
        ];
        let config = LlmConfig::from_vars(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
        .expect("config");

        let links = links(&config);
        assert_eq!(
            links.iter().map(|l| l.model.as_str()).collect::<Vec<_>>(),
            ["gemini-3.5-flash", "gemini-3.6-flash"]
        );
        assert!(links.iter().all(|l| l.fallbacks.is_empty()));
        assert!(links.iter().all(|l| l.max_retries == 0));
    }

    /// Found by running the command on the machine this was written on: the
    /// fallback to a second Gemini model carries the stored key, and the line
    /// said it came from `GOOGLE_API_KEY`, which is set nowhere.
    #[test]
    fn a_fallback_to_the_same_provider_names_the_key_it_carries() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };

        let stored: &[(&str, &str)] = &[
            ("ANAMNESIS_LLM_PROVIDER", "google"),
            ("ANAMNESIS_LLM_API_KEY", "AQ.test-key"),
            ("ANTHROPIC_API_KEY", "sk-ant-test"),
            (
                "ANAMNESIS_LLM_FALLBACK_PROVIDERS",
                "google:gemini-3.6-flash, anthropic",
            ),
        ];
        let config = LlmConfig::from_vars(lookup(stored)).expect("config");
        let named: Vec<_> = links(&config)
            .iter()
            .map(|link| key_variable(&config, link, lookup(stored)))
            .collect();
        assert_eq!(
            named,
            [
                Some("ANAMNESIS_LLM_API_KEY"),
                Some("ANAMNESIS_LLM_API_KEY"),
                Some("ANTHROPIC_API_KEY"),
            ]
        );

        let own: &[(&str, &str)] = &[
            ("ANAMNESIS_LLM_PROVIDER", "google"),
            ("GEMINI_API_KEY", "AQ.test-key"),
            ("ANAMNESIS_LLM_FALLBACK_PROVIDERS", "ollama:qwen2.5"),
        ];
        let config = LlmConfig::from_vars(lookup(own)).expect("config");
        let named: Vec<_> = links(&config)
            .iter()
            .map(|link| key_variable(&config, link, lookup(own)))
            .collect();
        assert_eq!(named, [Some("GEMINI_API_KEY"), None]);
    }

    /// Against a socket, the way the server would send it: the body Google
    /// sent on 2026-09-14 comes back as a refused key, in one request.
    #[test]
    fn a_refused_key_is_found_in_one_request() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = requests.clone();
        std::thread::spawn(move || {
            let body = "[{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Please pass a valid API key\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n]";
            for socket in listener.incoming() {
                let Ok(mut socket) = socket else { continue };
                let mut buffer = [0_u8; 16_384];
                let _ = socket.read(&mut buffer);
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = socket.write_all(
                    format!(
                        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });

        let base = format!("http://{address}/v1beta/openai");
        let pairs = [
            ("ANAMNESIS_LLM_PROVIDER", "google"),
            ("ANAMNESIS_LLM_API_KEY", "AQ.revoked-key-not-a-real-one"),
            ("ANAMNESIS_LLM_BASE_URL", base.as_str()),
            ("ANAMNESIS_LLM_MAX_RETRIES", "8"),
        ];
        let config = LlmConfig::from_vars(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
        .expect("config");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let verdict = runtime.block_on(ask(&links(&config)[0]));

        assert_eq!(
            verdict,
            Verdict::KeyRefused {
                status: 400,
                message: "Please pass a valid API key".to_owned()
            }
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }
}
