//! The same job as the deterministic path, done by a model.
//!
//! Everything here is arranged around one rule: a model may improve the page,
//! and it may never be the reason there isn't one. So this module has exactly
//! one entry point that the pipeline calls, [`consolidate_with_llm`], and that
//! function cannot fail — it returns the deterministic digest whenever the
//! model is unreachable, slow, declined, or answers with something that is not
//! a page. The failure is logged, not propagated.
//!
//! The prompt is *bounded*, not "usually short". A session that ran for six
//! hours produces far more material than any context window, and the failure
//! mode of ignoring that is the worst one available: consolidation works fine
//! in testing and then starts 400-ing on exactly the long sessions whose
//! summaries are worth the most.

use std::collections::VecDeque;

use anamnesis_core::observation::{EventKind, Observation};
use anamnesis_core::session::Session;
use anamnesis_llm::{Completion, Provider, clip_to_tokens, estimate_tokens};
use serde_json::{Value, json};

use crate::{HANDOFF_LIMIT, SessionDigest, clip, clip_bytes, consolidate};

/// Wiki page holding project-specific consolidation preferences.
///
/// A project that wants its summaries to mention ticket numbers, or to be
/// written in Turkish, or to always list migrations separately, says so here
/// rather than in anamnesis's source. The page is optional and its content is
/// treated as guidance, not as instructions that can restructure the reply —
/// the schema does that.
pub const PREFERENCES_PAGE: &str = "_prompts/consolidation.md";

/// Longest title accepted from a model, in characters, before the date.
const MAX_TITLE_CHARS: usize = 72;

/// Share of the prompt budget the preferences page may take.
///
/// Capped because it is user-editable and unbounded: a preferences page that
/// grew to five thousand words would otherwise crowd out the session it is
/// meant to describe.
const PREFERENCES_SHARE: usize = 5;

/// Longest single observation body included in the prompt, in characters.
const MAX_BODY_CHARS: usize = 600;

/// What the model is being asked to do.
///
/// Written as a job description rather than a list of prohibitions. The two
/// rules that carry their weight are "only what the observations support" —
/// because a plausible invention in a memory page is worse than an omission,
/// and will be read as fact by every later session — and the language rule,
/// because a summary written in a different language than the work is a
/// summary nobody rereads.
const SYSTEM: &str = "\
You compile durable memory for AI coding agents.

You are given the sanitized, recorded observations of one finished agent \
session: the prompts a person wrote, the tools the agent ran, and whether \
they succeeded. From that, you write the page that session leaves behind and \
the handoff the next session will read before it starts working.

What you are for is the part counting cannot reach. A tally of tool calls \
already exists and is not what is wanted here. Say what the session was \
trying to do, what it established, what it changed, what it ran into, and \
what is still open.

Rules:
- Only state what the observations support. If intent has to be inferred, \
infer it, and word it so a reader can tell it was inferred. Never invent a \
file, a command, a decision, or an outcome that is not there.
- Failed tool calls are the most useful thing in a transcript. Say what \
failed and, if it is visible, why.
- Name files and identifiers exactly as they appear.
- Write in the language the person wrote their prompts in.
- The handoff is read by an agent that has no other context and a limited \
budget for it. It is prose, not headings, and it says what to know and what \
to do next — not what happened, except where that changes what to do.
- If the session genuinely did nothing of substance, say so plainly and \
briefly rather than inflating it.";

/// The reply shape.
///
/// Constraining this at the API level is what removes the entire category of
/// "the model wrapped the answer in a friendly paragraph" from the failure
/// modes this path has to survive.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {
                "type": "string",
                "description": "What the session was about, in under 72 characters. No date — one is added.",
            },
            "body": {
                "type": "string",
                "description": "The session page, in markdown. Use ## for sections; no level-1 heading.",
            },
            "handoff": {
                "type": "string",
                "description": "Plain prose for the next session. Under 1500 characters.",
            },
        },
        "required": ["title", "body", "handoff"],
        "additionalProperties": false,
    })
}

/// Consolidate a session, preferring the model and falling back to counting.
///
/// Returns `None` only when there was nothing to record — the same condition
/// [`consolidate`] uses. Every other outcome is a digest.
pub async fn consolidate_with_llm(
    provider: &dyn Provider,
    session: &Session,
    observations: &[Observation],
    preferences: Option<&str>,
    max_input_tokens: usize,
    max_output_tokens: u32,
) -> Option<SessionDigest> {
    // The deterministic digest is computed first and unconditionally. It costs
    // microseconds, it decides whether this session is worth a page at all,
    // and holding it means the fallback below is a value rather than another
    // thing that can go wrong.
    let fallback = consolidate(session, observations)?;

    let request = Completion {
        system: SYSTEM.to_owned(),
        user: render_prompt(session, observations, preferences, max_input_tokens),
        schema: schema(),
        max_output_tokens,
    };

    match provider.complete(&request).await {
        Ok(output) => match digest_from_json(&output.json, session) {
            Ok(digest) => {
                tracing::info!(
                    provider = provider.name(),
                    model = %output.model,
                    input_tokens = output.input_tokens,
                    output_tokens = output.output_tokens,
                    "session consolidated by model"
                );
                Some(digest)
            }
            Err(reason) => {
                tracing::warn!(%reason, "model reply was not a page; using the counted summary");
                Some(fallback)
            }
        },
        Err(error) => {
            tracing::warn!(%error, "model unavailable; using the counted summary");
            Some(fallback)
        }
    }
}

/// Render the material for one session, inside a token budget.
pub fn render_prompt(
    session: &Session,
    observations: &[Observation],
    preferences: Option<&str>,
    max_tokens: usize,
) -> String {
    let mut out = String::new();

    out.push_str("# Session\n\n");
    out.push_str(&format!("- Agent: {}\n", session.agent));
    out.push_str(&format!("- Started: {}\n", session.started_at));
    if let Some(ended) = session.ended_at {
        out.push_str(&format!("- Ended: {ended}\n"));
    }
    out.push_str(&format!("- Working directory: {}\n", session.checkout_path.display()));
    out.push_str(&format!("- Events recorded: {}\n", observations.len()));

    if let Some(text) = preferences.map(str::trim).filter(|t| !t.is_empty()) {
        let share = max_tokens / PREFERENCES_SHARE;
        out.push_str("\n# Project preferences\n\n");
        out.push_str(&clip_to_tokens(text, share));
        out.push('\n');
    }

    out.push_str("\n# Transcript\n\n");

    // Whatever the header and preferences took is gone; the transcript gets
    // the rest. Subtracting the actual cost rather than a guess is what keeps
    // a long preferences page from silently pushing the total over.
    let spent = estimate_tokens(&out);
    let remaining = max_tokens.saturating_sub(spent);

    let lines: Vec<String> = observations.iter().map(render_observation).collect();
    for line in fit_lines(lines, remaining) {
        out.push_str(&line);
        out.push('\n');
    }

    out
}

/// One observation as one line.
fn render_observation(observation: &Observation) -> String {
    let time = observation.at.to_string();
    let time = time
        .split('T')
        .nth(1)
        .and_then(|t| t.split('.').next())
        .unwrap_or("--:--:--");

    let mut line = format!("[{time}] {}", observation.kind.as_str());

    if let Some(tool) = &observation.tool {
        line.push_str(&format!(" {}", tool.name));
        // Only failures are annotated. Marking every success would spend a
        // third of the transcript's budget restating the default.
        if tool.ok == Some(false) {
            line.push_str(" (FAILED)");
        }
    }

    let body = observation.body.as_str().trim();
    if !body.is_empty() {
        let limit = if observation.kind == EventKind::UserPrompt {
            MAX_BODY_CHARS
        } else {
            MAX_BODY_CHARS / 2
        };
        // Newlines would break the one-line-per-event shape the model is
        // reading, and the shape is what makes a long transcript legible.
        let flattened = clip(body, limit).replace(['\n', '\r'], " ⏎ ");
        line.push_str(&format!(": {flattened}"));
    }

    if observation.body.is_truncated() {
        line.push_str(" […]");
    }

    line
}

/// Drop events from the middle until the transcript fits.
///
/// From the middle, not the end, and not the start. How a session opened is
/// what its title and intent come from; how it ended is what the handoff is
/// about. The part that survives least well under compression is the long
/// grind in between, which is also the part the counted summary already
/// covers.
fn fit_lines(lines: Vec<String>, budget: usize) -> Vec<String> {
    let cost = |line: &String| estimate_tokens(line) + 1;
    let total: usize = lines.iter().map(cost).sum();
    if total <= budget {
        return lines;
    }

    // Room for the marker, so disclosing the omission cannot itself overflow.
    let budget = budget.saturating_sub(24);

    let mut head: Vec<String> = Vec::new();
    let mut tail: VecDeque<String> = VecDeque::new();
    let mut spent = 0;
    let mut front = 0;
    let mut back = lines.len();
    let mut prefer_front = true;

    while front < back {
        let index = if prefer_front { front } else { back - 1 };
        if spent + cost(&lines[index]) > budget {
            // One oversized line on this side does not mean the other side is
            // out of room too; only give up when neither end fits.
            let other = if prefer_front { back - 1 } else { front };
            if front == back - 1 || spent + cost(&lines[other]) > budget {
                break;
            }
            prefer_front = !prefer_front;
            continue;
        }

        spent += cost(&lines[index]);
        if prefer_front {
            head.push(lines[index].clone());
            front += 1;
        } else {
            tail.push_front(lines[index].clone());
            back -= 1;
        }
        prefer_front = !prefer_front;
    }

    let omitted = back.saturating_sub(front);
    let mut out = head;
    if omitted > 0 {
        out.push(format!("[… {omitted} events omitted to fit the context …]"));
    }
    out.extend(tail);
    out
}

/// Validate a model reply and shape it into a digest.
///
/// The error type is a plain string because it has exactly one consumer: a log
/// line explaining why the counted summary was used instead.
fn digest_from_json(value: &Value, session: &Session) -> Result<SessionDigest, String> {
    let field = |name: &str| -> Result<String, String> {
        let text = value
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("reply has no string field {name:?}"))?
            .trim()
            .to_owned();
        if text.is_empty() {
            return Err(format!("reply field {name:?} was empty"));
        }
        Ok(text)
    };

    let title = field("title")?;
    let body = field("body")?;
    let handoff = field("handoff")?;

    // Titles carry the date so that a directory listing sorts by time and so
    // that model-written and counted pages look alike. The model is told not
    // to add one; stripping a leading date it added anyway is cheaper than a
    // retry, and beats "2026-08-20: 2026-08-20: …".
    let date = session.started_at.to_string();
    let date = date.split('T').next().unwrap_or("undated").to_owned();
    let title = title
        .strip_prefix(&date)
        .map(|rest| rest.trim_start_matches([':', '-', ' ']))
        .unwrap_or(&title)
        .trim();
    let title = if title.is_empty() {
        format!("{date}: {} session", session.agent)
    } else {
        format!("{date}: {}", clip(title, MAX_TITLE_CHARS))
    };

    // A level-1 heading in the body duplicates the frontmatter title and
    // renders as a second title in every wiki viewer.
    let body = body
        .strip_prefix("# ")
        .and_then(|rest| rest.split_once('\n'))
        .map(|(_, rest)| rest.trim_start().to_owned())
        .unwrap_or(body);

    // The handoff budget is not advisory: it is injected into the next
    // session's context, where every byte competes with the work itself.
    let handoff = clip_bytes(handoff.trim(), HANDOFF_LIMIT);

    Ok(SessionDigest {
        title,
        body,
        handoff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::{ObservationId, ProjectId, SessionId, WorkspaceId};
    use anamnesis_core::observation::{BoundedBody, ToolRef};
    use anamnesis_core::session::{AgentKind, SessionState};
    use anamnesis_llm::{CompletionOutput, LlmError};
    use async_trait::async_trait;

    fn session() -> Session {
        Session {
            id: SessionId::new(),
            agent: AgentKind::ClaudeCode,
            workspace_id: WorkspaceId::from_uuid(uuid::Uuid::nil()),
            project_id: ProjectId::from_uuid(uuid::Uuid::nil()),
            checkout_path: "/repo".into(),
            started_at: "2026-08-20T09:00:00Z".parse().expect("timestamp"),
            ended_at: Some("2026-08-20T10:00:00Z".parse().expect("timestamp")),
            state: SessionState::Closed,
        }
    }

    fn observation(kind: EventKind, body: &str, tool: Option<ToolRef>) -> Observation {
        Observation {
            id: ObservationId::new(),
            session_id: SessionId::new(),
            kind,
            tool,
            at: "2026-08-20T09:30:00Z".parse().expect("timestamp"),
            body: BoundedBody::truncating(body, BoundedBody::DEFAULT_LIMIT),
            sanitized: false,
        }
    }

    fn working_session() -> Vec<Observation> {
        vec![
            observation(EventKind::SessionStart, "", None),
            observation(EventKind::UserPrompt, "add the llm provider", None),
            observation(
                EventKind::ToolUse,
                "crates/anamnesis-llm/src/lib.rs",
                Some(ToolRef {
                    name: "Write".to_owned(),
                    ok: Some(true),
                }),
            ),
            observation(
                EventKind::ToolUse,
                "cargo test",
                Some(ToolRef {
                    name: "Bash".to_owned(),
                    ok: Some(false),
                }),
            ),
        ]
    }

    /// A provider that answers however the test says, without a socket.
    struct Fake(Result<Value, ()>);

    #[async_trait]
    impl Provider for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-1"
        }
        async fn complete(&self, _: &Completion) -> Result<CompletionOutput, LlmError> {
            match &self.0 {
                Ok(json) => Ok(CompletionOutput {
                    json: json.clone(),
                    model: "fake-1".to_owned(),
                    input_tokens: 1,
                    output_tokens: 1,
                }),
                Err(()) => Err(LlmError::Config("no".to_owned())),
            }
        }
    }

    fn good_reply() -> Value {
        json!({
            "title": "LLM provider added",
            "body": "## What happened\n\nThe provider crate was written.",
            "handoff": "The provider exists; `cargo test` failed once and was not rerun.",
        })
    }

    #[tokio::test]
    async fn a_model_reply_becomes_the_page() {
        let digest = consolidate_with_llm(
            &Fake(Ok(good_reply())),
            &session(),
            &working_session(),
            None,
            6_500,
            2_000,
        )
        .await
        .expect("a digest");

        assert_eq!(digest.title, "2026-08-20: LLM provider added");
        assert!(digest.body.contains("What happened"));
        assert!(digest.handoff.contains("cargo test"));
    }

    #[tokio::test]
    async fn a_dead_provider_still_produces_a_page() {
        let digest = consolidate_with_llm(
            &Fake(Err(())),
            &session(),
            &working_session(),
            None,
            6_500,
            2_000,
        )
        .await
        .expect("a digest");

        // The counted page, verbatim — including the footer that says so.
        assert!(digest.body.contains("Compiled without a model"));
    }

    #[tokio::test]
    async fn a_reply_missing_a_field_falls_back_rather_than_writing_half_a_page() {
        let digest = consolidate_with_llm(
            &Fake(Ok(json!({"title": "t", "body": "b"}))),
            &session(),
            &working_session(),
            None,
            6_500,
            2_000,
        )
        .await
        .expect("a digest");

        assert!(digest.body.contains("Compiled without a model"));
    }

    #[tokio::test]
    async fn an_empty_session_gets_no_page_even_with_a_model() {
        let boundaries = [
            observation(EventKind::SessionStart, "", None),
            observation(EventKind::SessionEnd, "", None),
        ];
        assert!(
            consolidate_with_llm(
                &Fake(Ok(good_reply())),
                &session(),
                &boundaries,
                None,
                6_500,
                2_000,
            )
            .await
            .is_none()
        );
    }

    #[test]
    fn a_date_the_model_added_anyway_is_not_repeated() {
        let reply = json!({
            "title": "2026-08-20: LLM provider added",
            "body": "b",
            "handoff": "h",
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert_eq!(digest.title, "2026-08-20: LLM provider added");
    }

    #[test]
    fn a_level_one_heading_is_removed_so_the_title_is_not_shown_twice() {
        let reply = json!({
            "title": "t",
            "body": "# t\n\n## Details\n\ntext",
            "handoff": "h",
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert!(digest.body.starts_with("## Details"));
    }

    #[test]
    fn an_overlong_handoff_is_cut_to_the_budget() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "x".repeat(HANDOFF_LIMIT * 2),
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert!(digest.handoff.len() <= HANDOFF_LIMIT);
    }

    #[test]
    fn a_whitespace_only_field_is_rejected() {
        let reply = json!({"title": "   ", "body": "b", "handoff": "h"});
        assert!(digest_from_json(&reply, &session()).is_err());
    }

    #[test]
    fn the_prompt_names_the_files_and_failures_the_model_needs() {
        let prompt = render_prompt(&session(), &working_session(), None, 6_500);
        assert!(prompt.contains("add the llm provider"));
        assert!(prompt.contains("crates/anamnesis-llm/src/lib.rs"));
        assert!(prompt.contains("(FAILED)"));
        assert!(prompt.contains("Working directory"));
    }

    #[test]
    fn preferences_are_included_but_cannot_take_the_whole_budget() {
        let preferences = "ticket numbers matter. ".repeat(2_000);
        let prompt = render_prompt(&session(), &working_session(), Some(&preferences), 1_000);
        assert!(prompt.contains("Project preferences"));
        assert!(prompt.contains("ticket numbers matter"));
        assert!(prompt.contains("add the llm provider"), "transcript survived");
        assert!(estimate_tokens(&prompt) <= 1_100, "prompt stayed bounded");
    }

    #[test]
    fn a_long_session_keeps_its_beginning_and_its_end() {
        let mut observations = vec![observation(EventKind::UserPrompt, "FIRST PROMPT", None)];
        for index in 0..500 {
            observations.push(observation(
                EventKind::ToolUse,
                &format!("middle event number {index} with some padding text"),
                Some(ToolRef {
                    name: "Read".to_owned(),
                    ok: Some(true),
                }),
            ));
        }
        observations.push(observation(EventKind::UserPrompt, "LAST PROMPT", None));

        let prompt = render_prompt(&session(), &observations, None, 1_200);
        assert!(prompt.contains("FIRST PROMPT"));
        assert!(prompt.contains("LAST PROMPT"));
        assert!(prompt.contains("events omitted"));
        assert!(estimate_tokens(&prompt) <= 1_300);
    }

    #[test]
    fn a_transcript_that_fits_is_not_disturbed() {
        let prompt = render_prompt(&session(), &working_session(), None, 6_500);
        assert!(!prompt.contains("events omitted"));
    }

    #[test]
    fn newlines_in_a_prompt_do_not_break_the_one_line_per_event_shape() {
        let observations = vec![observation(
            EventKind::UserPrompt,
            "first line\nsecond line",
            None,
        )];
        let rendered = render_observation(&observations[0]);
        assert!(!rendered.contains('\n'));
        assert!(rendered.contains("second line"));
    }
}
