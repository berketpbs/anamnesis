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

use anamnesis_llm::{Completion, Provider};
use serde_json::{Value, json};

/// Most words an abstract may run to.
///
/// One sentence, as ai-memory asks of its own. Forty words is several times a
/// sentence that says what a page covers and well inside any embedding model's
/// window; a reply longer than this is a summary, and a summary embedded as one
/// vector is the long-page problem again at a smaller size.
pub const MAX_ABSTRACT_WORDS: usize = 40;

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
) -> Result<String, String> {
    let request = Completion {
        system: SYSTEM.to_owned(),
        user: abstract_prompt(title, body),
        schema: abstract_schema(),
        max_output_tokens,
    };
    let reply = provider
        .complete(&request)
        .await
        .map_err(|error| error.to_string())?;
    let text = reply
        .json
        .get("abstract")
        .and_then(Value::as_str)
        .ok_or_else(|| "the reply has no string field \"abstract\"".to_owned())?;
    check_abstract(text, title)
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
        assert!(error.contains("abstract"), "{error}");
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
