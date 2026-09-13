//! Asking a model for a page's abstract: one line saying what the page is
//! about.
//!
//! The model is shown the page and nothing else — no question it might be
//! asked, no other page, no session. That is the property this module exists to
//! keep. An abstract is what the abstract stream ranks a page by, and a line
//! written by something that had read the questions a suite asks would be an
//! answer placed where only that stream can see it: the measurement would score
//! the writer, not the stream. The same holds for a live page, where the
//! question is whatever somebody asks next.
//!
//! What comes back is checked rather than trusted, because every way an
//! abstract goes wrong is quiet. A heading, a bullet, a paragraph, or the title
//! said again all embed without complaint and rank a page by something other
//! than what it is about.

use anamnesis_llm::{Completion, LlmError, Provider};
use serde_json::{Value, json};

/// Why no abstract came back.
#[derive(Debug)]
pub enum AbstractError {
    /// The model was not reached, or answered with an error.
    Model(LlmError),
    /// The model answered, and the answer was not an abstract.
    Unusable(String),
}

impl AbstractError {
    /// Whether asking again later could succeed: a rate limit, an overloaded
    /// model, a connection that did not complete.
    ///
    /// A caller working through many pages stops on this rather than going
    /// on, since every page after it would meet the same limit and spend a
    /// request finding that out.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Model(error) => error.is_retryable() && !matches!(error, LlmError::Malformed(_)),
            Self::Unusable(_) => false,
        }
    }
}

impl std::fmt::Display for AbstractError {
    /// One line. A provider's error body can run to forty lines of JSON, and
    /// what somebody reading a list of pages needs from it is the status and
    /// the sentence.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(LlmError::Api {
                status, message, ..
            }) => write!(f, "the model answered {status}: {}", brief(message)),
            Self::Model(error) => write!(f, "{}", brief(&error.to_string())),
            Self::Unusable(reason) => write!(f, "{reason}"),
        }
    }
}

/// The human sentence out of a provider's error body, or the body's first line.
fn brief(message: &str) -> String {
    fn find(value: &Value) -> Option<String> {
        match value {
            Value::Object(map) => map
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| map.values().find_map(find)),
            Value::Array(items) => items.iter().find_map(find),
            _ => None,
        }
    }
    // The body is not always JSON on its own: the provider layer appends
    // "(retry after 29s)" to it, and the whole no longer parses. So the first
    // JSON value is read from where it starts, and whatever follows is left.
    let parsed = message
        .find(['[', '{'])
        .and_then(|start| {
            serde_json::Deserializer::from_str(&message[start..])
                .into_iter::<Value>()
                .next()
                .and_then(Result::ok)
        })
        .and_then(|value| find(&value));
    let text = parsed.unwrap_or_else(|| message.to_owned());
    let line = text.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(200) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

/// Most words an abstract may run to.
///
/// One sentence, as ai-memory asks of its own. Forty words is several times a
/// sentence that says what a page covers and well inside any embedding model's
/// window; a reply longer than this is a summary, and a summary embedded as one
/// vector is the long-page problem again at a smaller size.
pub const MAX_ABSTRACT_WORDS: usize = 40;

/// Words that, after "this" or "the", make an opening about the page itself.
const ABOUT_THE_PAGE: &[&str] = &[
    "page",
    "document",
    "log",
    "report",
    "guide",
    "note",
    "notes",
    "entry",
    "article",
    "session",
    "postmortem",
    "procedure",
    "runbook",
    "record",
    "decision",
    "file",
];

/// Longest page body sent, in characters.
///
/// The pages this is run over are wiki pages, and the longest in this project's
/// own memory is a few thousand words. The cap is for the page nobody expected:
/// what it keeps is the start, and the reply is still an abstract of what was
/// shown — said, where it happens, rather than passed off as the whole.
const MAX_BODY_CHARS: usize = 60_000;

const SYSTEM: &str = "You write the abstract of one wiki page: a single plain \
sentence saying what the page is about, so that someone searching later can \
tell from that sentence alone whether this is the page they need.\n\n\
Rules:\n\
- One sentence, at most 40 words, on one line.\n\
- Begin with the subject itself. Never begin with words about the page, such \
as \"This page\", \"This log\", \"This report\" or \"This guide\".\n\
- Say what the page covers as a whole, including what it reaches further down, \
not only how it opens.\n\
- Plain prose. No heading, no bullet, no list, no quotation marks around the \
sentence, no markdown.\n\
- Do not repeat the title as the sentence.\n\
- Use only what the page says. Do not add facts, advice, or judgement.";

/// The shape of the reply.
pub fn abstract_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "abstract": {
                "type": "string",
                "description": "One plain sentence, at most 40 words, saying what the page is about."
            }
        },
        "required": ["abstract"],
        "additionalProperties": false
    })
}

/// The material for one page: its title and body, and nothing else.
pub fn abstract_prompt(title: &str, body: &str) -> String {
    let (body, cut) = match body.char_indices().nth(MAX_BODY_CHARS) {
        Some((end, _)) => (&body[..end], true),
        None => (body, false),
    };
    let mut prompt = format!("Title: {title}\n\n{body}");
    if cut {
        prompt.push_str("\n\n[The page continues; only its start is shown.]");
    }
    prompt
}

/// Ask for a page's abstract, and check it before handing it back.
///
/// One request. A reply that fails the check is an error rather than a second
/// request: the models this runs against are on daily quotas, and a retry
/// spends one on the same instructions that produced the first reply.
pub async fn write_abstract(
    provider: &dyn Provider,
    title: &str,
    body: &str,
    max_output_tokens: u32,
) -> Result<String, AbstractError> {
    let request = Completion {
        system: SYSTEM.to_owned(),
        user: abstract_prompt(title, body),
        schema: abstract_schema(),
        max_output_tokens,
    };
    let reply = provider
        .complete(&request)
        .await
        .map_err(AbstractError::Model)?;
    let text = reply
        .json
        .get("abstract")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AbstractError::Unusable("the reply has no string field \"abstract\"".to_owned())
        })?;
    check_abstract(text, title).map_err(AbstractError::Unusable)
}

/// Whether a line is an abstract, and the line as it should be stored.
///
/// Whitespace is collapsed to single spaces: a line break inside an abstract is
/// a formatting accident, not content, and the frontmatter holds one line.
pub fn check_abstract(text: &str, title: &str) -> Result<String, String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_matches(|c: char| c == '"' || c == '\'').trim();

    if line.is_empty() {
        return Err("the abstract is empty".to_owned());
    }
    if line.starts_with('#') || line.starts_with("- ") || line.starts_with("* ") {
        return Err(format!("the abstract is a heading or a bullet: {line:?}"));
    }
    // Found on the first run over `long`: eight abstracts of eight began "This
    // page details", "This log details", "This report details". Words every
    // abstract shares are a component every abstract vector shares, and they
    // make the lines less distinguishable by exactly the stream built on them.
    // "This" and not "the": "the log partition wears out" is about a log
    // partition, while "this log details" is about the page.
    let lower = line.to_lowercase();
    let mut words = lower.split(' ');
    let about_the_page = match (words.next(), words.next()) {
        (Some("this"), Some(noun)) => ABOUT_THE_PAGE.contains(&noun),
        (Some("the"), Some("page" | "document")) => true,
        _ => false,
    };
    if about_the_page {
        return Err(format!(
            "the abstract begins by talking about the page rather than its subject: {line:?}"
        ));
    }
    let words = line.split(' ').count();
    if words > MAX_ABSTRACT_WORDS {
        return Err(format!(
            "the abstract runs to {words} words, more than {MAX_ABSTRACT_WORDS}: {line:?}"
        ));
    }
    let bare = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    if bare(line) == bare(title) {
        return Err(format!("the abstract only repeats the title: {line:?}"));
    }
    Ok(line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_llm::{CompletionOutput, LlmError};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A model that answers with one fixed reply and remembers what it was
    /// shown.
    struct Scripted {
        reply: Value,
        shown: Mutex<Vec<Completion>>,
    }

    #[async_trait]
    impl Provider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn model(&self) -> &str {
            "scripted-1"
        }
        async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError> {
            self.shown.lock().expect("lock").push(request.clone());
            Ok(CompletionOutput {
                json: self.reply.clone(),
                model: "scripted-1".to_owned(),
                input_tokens: 1,
                output_tokens: 1,
            })
        }
    }

    fn scripted(reply: Value) -> Scripted {
        Scripted {
            reply,
            shown: Mutex::new(Vec::new()),
        }
    }

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    /// The model sees the page and nothing else. Everything this module is for
    /// rests on that, so it is checked on the request itself.
    #[test]
    fn the_model_is_shown_the_page_and_nothing_else() {
        let model = scripted(json!({"abstract": "How the relay keeps its clock."}));

        let line = run(write_abstract(
            &model,
            "Relay clock",
            "The relay syncs at dawn.",
            500,
        ))
        .expect("abstract");

        assert_eq!(line, "How the relay keeps its clock.");
        let shown = model.shown.lock().expect("lock");
        assert_eq!(shown.len(), 1, "one request, no retry");
        assert_eq!(
            shown[0].user,
            "Title: Relay clock\n\nThe relay syncs at dawn."
        );
    }

    #[test]
    fn a_reply_without_the_field_is_an_error() {
        let model = scripted(json!({"summary": "How the relay keeps its clock."}));
        let error = run(write_abstract(&model, "t", "b", 500)).expect_err("no field");
        assert!(error.to_string().contains("abstract"), "{error}");
        assert!(
            !error.is_transient(),
            "asking again gets the same shape back"
        );
    }

    /// A provider's error body is forty lines of JSON; a list of pages needs
    /// the status and the sentence. And a rate limit is transient — the one
    /// kind of failure that says to stop asking for now, rather than go on
    /// spending a request per page to meet it again.
    #[test]
    fn a_rate_limit_reads_as_one_line_and_says_to_stop() {
        let body = r#"[{
  "error": {
    "code": 429,
    "message": "You exceeded your current quota, please check your plan.\n* Quota exceeded for metric: requests, limit: 5",
    "status": "RESOURCE_EXHAUSTED"
  }
}]
 (retry after 29s)"#;
        let error = AbstractError::Model(LlmError::Api {
            status: 429,
            kind: "unknown".to_owned(),
            message: body.to_owned(),
        });

        assert_eq!(
            error.to_string(),
            "the model answered 429: You exceeded your current quota, please check your plan."
        );
        assert!(error.is_transient());
        assert!(
            !AbstractError::Model(LlmError::Api {
                status: 401,
                kind: "auth".to_owned(),
                message: "bad key".to_owned(),
            })
            .is_transient()
        );
    }

    /// Every way an abstract goes wrong embeds without complaint, so each is
    /// refused here, where it can still be named.
    #[test]
    fn what_is_not_an_abstract_is_refused() {
        for (reply, why) in [
            ("   ", "empty"),
            ("## Relay clock", "heading"),
            ("- the relay syncs at dawn", "bullet"),
            ("Relay clock", "title"),
            ("relay CLOCK.", "title"),
        ] {
            assert!(
                check_abstract(reply, "Relay clock").is_err(),
                "{why}: {reply:?}"
            );
        }
        let long = "word ".repeat(MAX_ABSTRACT_WORDS + 1);
        assert!(check_abstract(&long, "t").is_err());
    }

    /// Eight of eight on the first real run began by describing the page.
    /// "The log partition" is a subject; "this log details" is not.
    #[test]
    fn an_opening_about_the_page_is_refused_and_a_subject_is_not() {
        for reply in [
            "This page details repairs to stations 12 through 16.",
            "This log details maintenance tasks.",
            "This report details an incident.",
            "The page explains the watchdog.",
        ] {
            assert!(check_abstract(reply, "t").is_err(), "{reply:?}");
        }
        for reply in [
            "The log partition wears out in eleven years at the old logging rate.",
            "This winter's brownouts came from cold batteries.",
            "Stations behind the valley gateway go silent during tower maintenance.",
        ] {
            assert!(check_abstract(reply, "t").is_ok(), "{reply:?}");
        }
    }

    /// A line break or surrounding quotes are formatting, not content: the
    /// frontmatter holds one line, and the line is kept.
    #[test]
    fn an_abstract_is_stored_as_one_clean_line() {
        assert_eq!(
            check_abstract("  \"How the relay\n keeps its clock.\"  ", "Relay clock")
                .expect("abstract"),
            "How the relay keeps its clock."
        );
    }

    /// A page past the cap is sent as its start and says so, rather than
    /// being passed off as the whole page.
    #[test]
    fn a_page_past_the_cap_is_marked_as_cut() {
        let body = "x".repeat(MAX_BODY_CHARS + 10);
        let prompt = abstract_prompt("t", &body);
        assert!(
            prompt.ends_with("only its start is shown.]"),
            "{}",
            &prompt[prompt.len() - 60..]
        );
        assert!(!abstract_prompt("t", "short").contains("continues"));
    }
}
