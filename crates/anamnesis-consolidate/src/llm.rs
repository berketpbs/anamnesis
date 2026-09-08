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
use anamnesis_core::page::{Entity, PagePath};
use anamnesis_core::session::Session;
use anamnesis_llm::{Completion, LlmError, Provider, clip_to_tokens, estimate_tokens};
use serde_json::{Value, json};

use crate::{
    HANDOFF_LIMIT, MAX_ENTITIES, MAX_NOTES, Note, NoteKind, SessionDigest, clip, clip_bytes,
    consolidate,
};

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

/// Share of the prompt the list of existing pages may take, as a divisor.
///
/// Smaller than the preferences share because paths are short and a project
/// with a thousand of them must not crowd out the session being summarised.
/// A truncated list is a page the model cannot link to, which costs one edge;
/// a truncated transcript is a session it cannot describe.
const PAGES_SHARE: usize = 8;

/// What the model is told about the memory this session is joining.
///
/// Bundled rather than passed one by one because both halves answer the same
/// question — what is already here — and because the caller that has one
/// almost always has the other.
#[derive(Debug, Clone, Copy, Default)]
pub struct Surroundings<'a> {
    /// The project's consolidation preferences, if it has written any.
    pub preferences: Option<&'a str>,
    /// Paths of pages already in this project's memory.
    ///
    /// The model is asked to link only to these. It cannot see the wiki, so a
    /// prompt that invites linking without saying what exists invites invented
    /// paths — links that resolve to nothing and mean nothing.
    pub pages: &'a [String],
}

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
- The title names this session, not its genre. Every page here is a session \
report, so `Session Summary`, `Session Handoff` and `Session Report` pick out \
none of them, and neither does an identifier copied out of a path. Do not \
begin the title with the word `Session`, and do not label it before saying \
it: write what this session was about, the way somebody scanning a directory \
listing would want it named.
- Write in the language the person wrote their prompts in.
- The handoff is read by an agent that has no other context and a limited \
budget for it. It is prose, not headings, and it says what to know and what \
to do next — not what happened, except where that changes what to do.
- If the session genuinely did nothing of substance, say so plainly and \
briefly rather than inflating it.
- The entities are what a later search would type to find this page: the \
files, crates, tools, systems, and error names this session was actually \
about, spelled as they appear. Name a file the way somebody would type it — \
`sanitize.rs`, not `crates/anamnesis-core/src/sanitize.rs` — because a search \
has to contain every word of the name. At most ten, fewer when fewer are \
warranted, and nothing generic — `code`, `bug`, and `session` find everything \
and therefore nothing.
- This memory already holds pages, and the ones it holds are listed for you. \
Where the session genuinely bears on one — it caused what that page describes, \
or fixed it, or contradicts it, or is the reason it was written — say so in \
the body and link the page by its path: `[[gotchas/a-page-name.md]]`. Link \
only to a path that appears in that list, exactly as it is written there. A \
link to a page that does not exist points nowhere, and a page that links to \
everything distinguishes nothing; when nothing is genuinely related, link \
nothing.
- Most sessions leave nothing behind that outlives them, and for those the \
notes are an empty list. A note is for the thing a later session would have to \
be told and could not work out from the code in front of it: a decision and \
what it was decided against, a gotcha — something that behaves differently \
than it looks like it does — or a procedure worth following again. It is not a \
second telling of the session page, not a description of the change, and not \
advice that would be true of any project. If nothing clears that bar, write \
none: a memory whose durable pages are mostly filler is one nobody reads far \
enough into to find the two that were real.
- A note's title is the claim it makes, so that a listing of them argues with \
somebody scanning it: `A moved crate breaks the Docker build`, not `Docker \
notes`. Do not label it with its own kind — it is already filed under one — \
and give it no date, because a decision is not found by when it was noticed.";

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
                "description": "What this session was about, in under 72 characters. Name the session, not the genre: every page here is a session report, so do not begin with \"Session\". No date — one is added.",
            },
            "body": {
                "type": "string",
                "description": "The session page, in markdown. Use ## for sections; no level-1 heading. A page already in this memory may be linked by its path, written exactly as the prompt lists it: `[[gotchas/a-page-name.md]]`. Only paths from that list.",
            },
            "handoff": {
                "type": "string",
                "description": "Plain prose for the next session. Under 1500 characters.",
            },
            "entities": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Up to 10 canonical names this session was about, spelled as they appear: files, crates, tools, systems, error names. A file is named without its directories — `sanitize.rs`, not `crates/anamnesis-core/src/sanitize.rs`. Nothing generic.",
            },
            "notes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["decision", "gotcha", "procedure"],
                        },
                        "title": {
                            "type": "string",
                            "description": "The claim this page makes, in under 72 characters. Not a label for its kind, and no date.",
                        },
                        "body": {
                            "type": "string",
                            "description": "The page, in markdown. Use ## for sections; no level-1 heading.",
                        },
                    },
                    "required": ["kind", "title", "body"],
                    "additionalProperties": false,
                },
                "description": "Durable pages this session leaves behind, at most 3. Usually empty: only what a later session would have to be told and could not work out from the code.",
            },
        },
        "required": ["title", "body", "handoff", "entities", "notes"],
        "additionalProperties": false,
    })
}

/// The same shape with the optional half removed.
///
/// Not a flag on [`schema`], because these are not two configurations of one
/// request: this one is what a session gets when the full answer would not
/// fit, and naming it says so at every call site.
pub fn schema_without_notes() -> Value {
    let mut reduced = schema();
    reduced["properties"]
        .as_object_mut()
        .expect("the schema is an object")
        .remove("notes");
    reduced["required"] = json!(["title", "body", "handoff", "entities"]);
    reduced
}

/// Which path produced a digest.
///
/// Capture does not need to ask: there is no page yet, and a counted one is
/// better than none. Recompiling an existing page is the opposite case — the
/// summary already there may have been written by a model, and replacing it
/// with counts is a loss no git history makes good in the moment somebody
/// reads it. So the two outcomes stop being interchangeable at the boundary
/// where a caller can act on the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestSource {
    /// A model read the session and wrote the page.
    Model,
    /// Counted, because the model was unavailable, refused, or answered with
    /// something that was not a page.
    Counted,
}

/// Consolidate a session, preferring the model and falling back to counting.
///
/// Returns `None` only when there was nothing to record — the same condition
/// [`consolidate`] uses. Every other outcome is a digest.
pub async fn consolidate_with_llm(
    provider: &dyn Provider,
    session: &Session,
    observations: &[Observation],
    surroundings: Surroundings<'_>,
    max_input_tokens: usize,
    max_output_tokens: u32,
) -> Option<SessionDigest> {
    consolidate_with_source(
        provider,
        session,
        observations,
        surroundings,
        max_input_tokens,
        max_output_tokens,
    )
    .await
    .map(|(digest, _)| digest)
}

/// The same, saying which path produced the digest.
///
/// For callers that are replacing something rather than writing the first
/// thing there.
pub async fn consolidate_with_source(
    provider: &dyn Provider,
    session: &Session,
    observations: &[Observation],
    surroundings: Surroundings<'_>,
    max_input_tokens: usize,
    max_output_tokens: u32,
) -> Option<(SessionDigest, DigestSource)> {
    // The deterministic digest is computed first and unconditionally. It costs
    // microseconds, it decides whether this session is worth a page at all,
    // and holding it means the fallback below is a value rather than another
    // thing that can go wrong.
    let fallback = consolidate(session, observations)?;

    let mut request = Completion {
        system: SYSTEM.to_owned(),
        user: render_prompt(session, observations, surroundings, max_input_tokens),
        schema: schema(),
        max_output_tokens,
    };

    let mut reply = provider.complete(&request).await;

    // A reply that did not fit is asked for again without the notes, once.
    //
    // This is the invariant this whole module is built on, arriving somewhere
    // it was not expected: the durable pages a session leaves are optional,
    // the page itself is not, and they are asked for in the same reply and so
    // share one output budget. A long session whose model wrote three extra
    // pages hits the ceiling, and *every* field is lost — the page becomes a
    // tally of tool calls because the model was generous about something that
    // was never required. Retrying the identical request cannot help; the
    // budget is the same and so is the answer. Dropping the optional half can,
    // and does: the session gets the reading it would have had before this
    // feature existed.
    //
    // Found by running it against a real session of 526 observations, which
    // had been summarised by a model until notes were added to the schema and
    // then fell back to counted twice in a row.
    if matches!(reply, Err(LlmError::Truncated(_))) {
        tracing::warn!(
            "the reply did not fit its output budget; asking again without the durable pages"
        );
        request.schema = schema_without_notes();
        reply = provider.complete(&request).await;
    }

    match reply {
        Ok(output) => match digest_from_json(&output.json, session) {
            Ok(digest) => {
                tracing::info!(
                    provider = provider.name(),
                    model = %output.model,
                    input_tokens = output.input_tokens,
                    output_tokens = output.output_tokens,
                    "session consolidated by model"
                );
                Some((digest, DigestSource::Model))
            }
            Err(reason) => {
                tracing::warn!(%reason, "model reply was not a page; using the counted summary");
                Some((fallback, DigestSource::Counted))
            }
        },
        Err(error) => {
            tracing::warn!(%error, "model unavailable; using the counted summary");
            Some((fallback, DigestSource::Counted))
        }
    }
}

/// Render the material for one session, inside a token budget.
pub fn render_prompt(
    session: &Session,
    observations: &[Observation],
    surroundings: Surroundings<'_>,
    max_tokens: usize,
) -> String {
    let preferences = surroundings.preferences;
    let mut out = String::new();

    out.push_str("# Session\n\n");
    out.push_str(&format!("- Agent: {}\n", session.agent));
    out.push_str(&format!("- Started: {}\n", session.started_at));
    if let Some(ended) = session.ended_at {
        out.push_str(&format!("- Ended: {ended}\n"));
    }
    out.push_str(&format!(
        "- Working directory: {}\n",
        session.checkout_path.display()
    ));
    out.push_str(&format!("- Events recorded: {}\n", observations.len()));

    if let Some(text) = preferences.map(str::trim).filter(|t| !t.is_empty()) {
        let share = max_tokens / PREFERENCES_SHARE;
        out.push_str("\n# Project preferences\n\n");
        out.push_str(&clip_to_tokens(text, share));
        out.push('\n');
    }

    if !surroundings.pages.is_empty() {
        out.push_str("\n# Pages already in this memory\n\n");
        out.push_str(&render_known_pages(
            surroundings.pages,
            max_tokens / PAGES_SHARE,
        ));
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

/// The paths a model may link to, as many as the budget holds.
///
/// Whole paths only. A clipped path is not a shorter path, it is a link that
/// resolves to nothing — so the budget is spent in path-sized units and the
/// remainder is reported rather than sliced. Saying how many were left out
/// matters: a model told to link only to what it can see should know that what
/// it can see is not everything.
fn render_known_pages(pages: &[String], max_tokens: usize) -> String {
    // Reserved for the line that says what was dropped, so adding it can never
    // be what pushes the section over.
    const MARKER_TOKENS: usize = 16;
    let budget = max_tokens.saturating_sub(MARKER_TOKENS);

    let mut out = String::new();
    let mut spent = 0;
    let mut shown = 0;
    for path in pages {
        let line = format!("- {path}\n");
        let cost = estimate_tokens(&line);
        if spent + cost > budget {
            break;
        }
        out.push_str(&line);
        spent += cost;
        shown += 1;
    }

    if shown < pages.len() {
        out.push_str(&format!(
            "[… {} more pages, not listed …]\n",
            pages.len() - shown
        ));
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

/// Labels a session page's title reaches for.
const SESSION_LABELS: [&str; 9] = [
    "session",
    "session report",
    "session summary",
    "session handoff",
    "session notes",
    "session log",
    "summary",
    "handoff",
    "report",
];

/// Labels a note's title reaches for.
///
/// The same failure one namespace down, and worse there: a `gotchas/` listing
/// where every entry opens with `Gotcha:` has spent the most valuable part of
/// each title restating the directory the reader is already standing in.
const NOTE_LABELS: [&str; 6] = [
    "decision",
    "gotcha",
    "procedure",
    "note",
    "lesson",
    "learning",
];

/// Drop a genre label the model wrote in front of the title.
///
/// The prompt tells it not to, and on a short session it complies. On a long
/// one — the sessions actually worth finding again — a small local model
/// reaches for `Session Report: …` anyway, and a directory listing where every
/// third entry opens with the same two words is the thing that rule exists to
/// prevent. Stripping it is cheaper than a retry, exactly as with the date.
///
/// Only a label *followed by a separator* goes. `Session to Address the Model
/// Lock` is a sentence about this session rather than a heading over it, and a
/// rule that cannot tell those apart would eat the title.
fn strip_genre_label<'a>(title: &'a str, labels: &[&str]) -> &'a str {
    // ':' and the dashes only. A hyphen belongs to `anamnesis-llm` far more
    // often than it separates a label from a title.
    let Some((head, rest)) = title.split_once([':', '—', '–']) else {
        return title;
    };
    let rest = rest.trim();
    if rest.is_empty() || !labels.contains(&head.trim().to_ascii_lowercase().as_str()) {
        return title;
    }
    rest
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
    // Newlines first: `unescape_quotes` decides what is code by looking for
    // fences at the start of a line, and a reply whose every break is still
    // the two characters `\` and `n` has no lines to look at.
    let body = unescape_quotes(&unescape_newlines(&field("body")?));
    let handoff = unescape_quotes(&unescape_newlines(&field("handoff")?));

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
    let title = strip_genre_label(title, &SESSION_LABELS);
    let title = if title.is_empty() {
        format!("{date}: {} session", session.agent)
    } else {
        format!("{date}: {}", clip(title, MAX_TITLE_CHARS))
    };

    let body = without_leading_heading(body);

    // The handoff budget is not advisory: it is injected into the next
    // session's context, where every byte competes with the work itself.
    let handoff = clip_bytes(handoff.trim(), HANDOFF_LIMIT);

    // Entities the model named, in its order, de-duplicated, with anything
    // unusable dropped rather than failing the reply: a name too long or
    // carrying a control character costs this page one search term, and
    // refusing the whole digest over it would cost the page itself.
    let entities = read_entities(value);

    // Same policy as the entities, and for the same reason: an unusable note
    // costs this session one durable page, while refusing the whole reply over
    // it would cost the page that was already written.
    let notes = read_notes(value);

    Ok(SessionDigest {
        title,
        body,
        handoff,
        entities,
        notes,
    })
}

/// Drop a level-1 heading a model wrote at the top of a page body.
///
/// It duplicates the frontmatter title and renders as a second title in every
/// wiki viewer. Shared by the session page and the notes because it is one
/// habit rather than two, and a fix that reached only one of them would show
/// up as whichever page the reader happened to open.
fn without_leading_heading(body: String) -> String {
    body.strip_prefix("# ")
        .and_then(|rest| rest.split_once('\n'))
        .map(|(_, rest)| rest.trim_start().to_owned())
        .unwrap_or(body)
}

/// Undo the escaping a model applied to prose it had already put in a string.
///
/// Seen in the wild, from a small local model: a handoff whose every paragraph
/// break is the two characters `\` and `n` rather than a newline. The JSON was
/// valid, the field was a non-empty string, and every check this module makes
/// passed — the text simply arrives as one unbroken wall, and a handoff is
/// injected into the next session's context exactly as written.
///
/// Only when there is **no** real newline in the whole string. A page that
/// already has line breaks and also writes the escape sequence is discussing
/// it, most likely in code, and rewriting that would corrupt the one thing it
/// was trying to say. Nothing else is unescaped: tabs and quotes are rarer,
/// more ambiguous, and were not the failure.
fn unescape_newlines(text: &str) -> String {
    const ESCAPED: &str = r"\n";
    const ESCAPED_CRLF: &str = r"\r\n";

    if text.contains('\n') || !text.contains(ESCAPED) {
        return text.to_owned();
    }
    text.replace(ESCAPED_CRLF, "\n").replace(ESCAPED, "\n")
}

/// Undo the quote escaping a model applied to prose it had already put in a
/// string.
///
/// The sibling of [`unescape_newlines`], found the same way — by reading the
/// pages this project has written about itself. Three of twenty session pages
/// carry `\"` where a quotation mark belongs:
///
/// ```text
/// Kullanicinin \"kur su scheduled task'i\" talebi uzerine
/// generic titles such as \"Session Summary\", \"Session Handoff\"
/// ```
///
/// Sixteen occurrences, every one of them prose, not one inside code.
///
/// `unescape_newlines` ruled this out on the grounds that quotes were "rarer,
/// more ambiguous, and were not the failure". Rarer is still true; the last
/// has stopped being true. The ambiguity is real but it has a shape: an
/// escaped quote in prose is a mistake, and an escaped quote inside code is
/// usually the point — so only what is outside a fence or an inline span is
/// repaired.
///
/// What is deliberately *not* carried over is the other guard. Newlines are
/// only unescaped in a string that has none of the real thing, because a wall
/// of text is the whole symptom there. Quotes have no such tell: all sixteen
/// are on pages whose paragraphs are perfectly intact, and that guard would
/// have skipped every one.
fn unescape_quotes(text: &str) -> String {
    if !text.contains(ESCAPED_QUOTE) {
        return text.to_owned();
    }

    let mut out = String::with_capacity(text.len());
    let mut fenced = false;

    for line in text.split_inclusive('\n') {
        let opener = line.trim_start();
        if opener.starts_with("```") || opener.starts_with("~~~") {
            fenced = !fenced;
            out.push_str(line);
        } else if fenced {
            out.push_str(line);
        } else {
            push_outside_code_spans(line, &mut out);
        }
    }

    out
}

/// The escape as it arrives: a backslash the model wrote itself, and the
/// quote it thought it was protecting.
const ESCAPED_QUOTE: &str = r#"\""#;

/// Copy one line, repairing quotes everywhere except inside inline code.
///
/// A span is a run of backticks closed by a run of the same length, which is
/// what makes ``a `` b`` work; an unclosed run is not a span, and the rest of
/// the line is prose.
fn push_outside_code_spans(line: &str, out: &mut String) {
    let mut rest = line;

    while let Some(at) = rest.find('`') {
        let (prose, from_tick) = rest.split_at(at);
        out.push_str(&prose.replace(ESCAPED_QUOTE, "\""));

        let ticks = from_tick.len() - from_tick.trim_start_matches('`').len();
        let (run, after) = from_tick.split_at(ticks);

        match after.find(run) {
            Some(end) => {
                let span = ticks + end + ticks;
                out.push_str(&from_tick[..span]);
                rest = &from_tick[span..];
            }
            None => {
                out.push_str(run);
                rest = after;
            }
        }
    }

    out.push_str(&rest.replace(ESCAPED_QUOTE, "\""));
}

/// A file entity, named the way a search would type it.
///
/// Entity matching wants *every* token of a name to appear in the query, so
/// `crates/anamnesis-core/src/sanitize.rs` asks a searcher for six tokens and
/// is therefore matched by nobody — while `sanitize.rs` asks for the two
/// somebody would actually write. The counted path has always known this and
/// files entities by basename; a model asked for names "spelled as they
/// appear" hands back the path it saw, and the two writers then build
/// different indexes from the same session.
///
/// Only a path is shortened, and only when its last segment looks like a
/// filename. A branch (`fix/redact-the-other-google-credential`) or a host
/// path (`github.com/berketpbs/anamnesis`) keeps every word, because there the
/// leading segments are the name rather than a place to find it.
fn shorten_path(name: &str) -> &str {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if last.len() < name.len() && last.contains('.') && !last.starts_with('.') {
        return last;
    }
    name
}

/// Entity names from a model reply, validated and bounded.
fn read_entities(value: &Value) -> Vec<Entity> {
    let mut entities: Vec<Entity> = Vec::new();
    let Some(named) = value.get("entities").and_then(Value::as_array) else {
        return entities;
    };

    for name in named.iter().filter_map(Value::as_str).map(shorten_path) {
        let Ok(entity) = Entity::parse(name) else {
            continue;
        };
        if !entities.contains(&entity) {
            entities.push(entity);
        }
        if entities.len() == MAX_ENTITIES {
            break;
        }
    }
    entities
}

/// The durable pages a model named, validated into notes.
///
/// Unusable entries are dropped rather than failing the reply, and each of the
/// three ways to be unusable is a page that could not have been written: a
/// kind that is not one of the three has no namespace to go in, a body that
/// says nothing is a title making a claim and then not supporting it, and a
/// title with no letters or digits in it leaves nothing a filename can be made
/// from.
///
/// Two notes that derive the same path are one note. A model that decides a
/// session left two gotchas and names them nearly the same way would otherwise
/// have the second overwrite the first *inside a single commit*, where no
/// history records that the first was ever written.
fn read_notes(value: &Value) -> Vec<Note> {
    let mut notes: Vec<Note> = Vec::new();
    let Some(listed) = value.get("notes").and_then(Value::as_array) else {
        return notes;
    };

    for entry in listed {
        let text = |name: &str| entry.get(name).and_then(Value::as_str).unwrap_or_default();

        let Some(kind) = NoteKind::parse(text("kind")) else {
            continue;
        };

        let title = clip(
            strip_genre_label(text("title").trim(), &NOTE_LABELS),
            MAX_TITLE_CHARS,
        );

        // The escapes the session page has to undo are undone here too. The
        // reply is one string from one model, and a habit that puts the two
        // characters `\` and `n` where a paragraph break belongs does not stop
        // at a field boundary.
        let body =
            without_leading_heading(unescape_quotes(&unescape_newlines(text("body").trim())));
        if body.trim().is_empty() {
            continue;
        }

        let Ok(path) = PagePath::derive(kind.namespace(), &title) else {
            continue;
        };
        if notes.iter().any(|note| note.path == path) {
            continue;
        }

        notes.push(Note {
            kind,
            path,
            title,
            body,
        });
        if notes.len() == MAX_NOTES {
            break;
        }
    }
    notes
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
            workstream_id: None,
            checkout_path: "/repo".into(),
            started_at: "2026-08-20T09:00:00Z".parse().expect("timestamp"),
            ended_at: Some("2026-08-20T10:00:00Z".parse().expect("timestamp")),
            state: SessionState::Closed,
            operator: None,
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
            Surroundings::default(),
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
            Surroundings::default(),
            6_500,
            2_000,
        )
        .await
        .expect("a digest");

        // The counted page, verbatim — including the footer that says so.
        assert!(digest.body.contains("Compiled without a model"));
    }

    /// The distinction a caller replacing an existing page has to be able to
    /// make. Both of these return a digest; only one of them read the session.
    #[tokio::test]
    async fn a_digest_says_whether_a_model_wrote_it() {
        let (_, from_model) = consolidate_with_source(
            &Fake(Ok(good_reply())),
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
            2_000,
        )
        .await
        .expect("a digest");
        assert_eq!(from_model, DigestSource::Model);

        let (_, from_counting) = consolidate_with_source(
            &Fake(Err(())),
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
            2_000,
        )
        .await
        .expect("a digest");
        assert_eq!(from_counting, DigestSource::Counted);
    }

    /// A reply the model did send but that could not be read as a page counts
    /// as counted too. It is the same loss to a page already written: prose
    /// replaced by tool tallies, with a commit to say it was deliberate.
    #[tokio::test]
    async fn a_reply_that_was_not_a_page_is_counted_rather_than_model() {
        let (digest, source) = consolidate_with_source(
            &Fake(Ok(json!({"title": "t", "body": "b"}))),
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
            2_000,
        )
        .await
        .expect("a digest");

        assert_eq!(source, DigestSource::Counted);
        assert!(digest.body.contains("Compiled without a model"));
    }

    #[tokio::test]
    async fn a_reply_missing_a_field_falls_back_rather_than_writing_half_a_page() {
        let digest = consolidate_with_llm(
            &Fake(Ok(json!({"title": "t", "body": "b"}))),
            &session(),
            &working_session(),
            Surroundings::default(),
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
                Surroundings::default(),
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

    /// The prompt forbids the label and a small model writes it anyway once
    /// the session is long enough. What must survive is the other shape: a
    /// title that opens with the same word as part of a sentence.
    #[test]
    fn a_genre_label_goes_but_a_title_that_merely_starts_with_the_word_stays() {
        let labelled = [
            ("Session Report: Memory analysis", "Memory analysis"),
            (
                "Session: Transitioning to info logs",
                "Transitioning to info logs",
            ),
            ("session summary — Ollama fallback", "Ollama fallback"),
            ("Handoff: what to do next", "what to do next"),
        ];
        for (written, wanted) in labelled {
            let reply = json!({"title": written, "body": "b", "handoff": "h"});
            let digest = digest_from_json(&reply, &session()).expect("a digest");
            assert_eq!(digest.title, format!("2026-08-20: {wanted}"), "{written}");
        }

        let kept = [
            // A sentence about the session, not a heading over it.
            "Session to address the model lock",
            // The label is where it belongs — at the end, saying what kind of
            // thing this was rather than announcing it.
            "Project hardening session",
            // A colon that separates a subject from its detail, not a label.
            "anamnesis-llm: the effort setting",
            // Nothing after the separator to promote.
            "Session Report:",
        ];
        for written in kept {
            let reply = json!({"title": written, "body": "b", "handoff": "h"});
            let digest = digest_from_json(&reply, &session()).expect("a digest");
            assert_eq!(digest.title, format!("2026-08-20: {written}"), "{written}");
        }
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
    fn the_names_a_model_gives_become_the_entities() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": ["Windows BOM", "anamnesis-llm", "tini"],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        let named: Vec<&str> = digest.entities.iter().map(Entity::as_str).collect();
        assert_eq!(named, vec!["Windows BOM", "anamnesis-llm", "tini"]);
    }

    /// Measured against a real model before this was written: Gemini answered
    /// with `crates/anamnesis-core/src/sanitize.rs`, which the entity stream
    /// can never match — it would want all six tokens in the query. The
    /// counted path has always filed the basename, so without this the two
    /// writers build different indexes out of the same session.
    #[test]
    fn a_file_entity_is_named_the_way_a_search_would_type_it() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [
                "crates/anamnesis-core/src/sanitize.rs",
                "crates\\anamnesis-llm\\src\\openai.rs",
            ],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        let named: Vec<&str> = digest.entities.iter().map(Entity::as_str).collect();
        assert_eq!(named, vec!["sanitize.rs", "openai.rs"]);
    }

    /// And only a path is shortened. These carry their leading segments as
    /// part of the name, so cutting them would leave a term nobody types
    /// either — the opposite mistake, made silently.
    #[test]
    fn a_name_that_only_looks_like_a_path_is_left_whole() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [
                "fix/redact-the-other-google-credential",
                "github.com/berketpbs/anamnesis",
                "sanitize.rs",
            ],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        let named: Vec<&str> = digest.entities.iter().map(Entity::as_str).collect();
        assert_eq!(
            named,
            vec![
                "fix/redact-the-other-google-credential",
                "github.com/berketpbs/anamnesis",
                "sanitize.rs",
            ]
        );
    }

    /// Two paths under one basename are one entity, not two — the same
    /// deduplication every other source of names goes through.
    #[test]
    fn two_paths_with_one_basename_are_one_entity() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": ["crates/anamnesis-llm/src/lib.rs", "crates/anamnesis-web/src/lib.rs"],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        let named: Vec<&str> = digest.entities.iter().map(Entity::as_str).collect();
        assert_eq!(named, vec!["lib.rs"]);
    }

    #[test]
    fn an_unusable_name_costs_one_search_term_not_the_page() {
        // A name too long, one that is only whitespace, one carrying a control
        // character, and one that is not a string at all. Refusing the whole
        // digest over any of them would throw away a page the model wrote
        // correctly.
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": ["x".repeat(200), "   ", "with\u{7}bell", "SQLite", 7],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        let named: Vec<&str> = digest.entities.iter().map(Entity::as_str).collect();
        assert_eq!(named, vec!["SQLite"]);
    }

    #[test]
    fn a_model_naming_the_same_thing_twice_names_it_once() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": ["SQLite", "SQLite", "sqlite"],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        // Spelling is preserved as written, so these are two names, not three.
        assert_eq!(digest.entities.len(), 2);
    }

    #[test]
    fn a_model_cannot_name_more_than_the_ceiling() {
        let many: Vec<String> = (0..40).map(|n| format!("thing{n}")).collect();
        let reply = json!({"title": "t", "body": "b", "handoff": "h", "entities": many});
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert_eq!(digest.entities.len(), MAX_ENTITIES);
    }

    /// Taken from a real reply: a local model escaped its own paragraph breaks,
    /// so the handoff arrived as one wall of text with `\n` printed in it. Every
    /// check here passed — the JSON was valid and the fields were non-empty
    /// strings — and the next session would have been handed that verbatim.
    #[test]
    fn a_model_that_escaped_its_own_newlines_gets_them_back() {
        let reply = json!({
            "title": "t",
            "body": r"Goal: ship it.\n\nDone: the provider.",
            "handoff": r"What to know:\n- it works\n\nWhat to do:\n1. check it",
            "entities": [],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert!(
            digest.body.contains("ship it.\n\nDone"),
            "{:?}",
            digest.body
        );
        assert!(!digest.handoff.contains(r"\n"), "{:?}", digest.handoff);
        assert_eq!(digest.handoff.lines().count(), 5);
    }

    /// The line this must not cross. A page with real line breaks that also
    /// writes the escape sequence is talking *about* it — almost always in
    /// code — and rewriting it would corrupt the one thing it was explaining.
    #[test]
    fn text_that_already_has_line_breaks_is_left_alone() {
        let body = "The parser splits on:\n\n    text.split('\\n')\n\nand keeps order.";
        let reply = json!({"title": "t", "body": body, "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert_eq!(digest.body, body, "an explanation of an escape survives it");
    }

    /// The failure as the wiki actually holds it. Three of this project's
    /// twenty session pages carry an escaped quote; this is one of them,
    /// copied out of `sessions/2026-08-31-823399e0.md`.
    #[test]
    fn a_quote_the_model_escaped_is_given_back() {
        let body = r#"Kullanicinin \"kur su scheduled task'i\" talebi uzerine baslandi."#;
        let reply = json!({"title": "t", "body": body, "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert_eq!(
            digest.body,
            "Kullanicinin \"kur su scheduled task'i\" talebi uzerine baslandi."
        );
    }

    /// The guard from `unescape_newlines` that must *not* be carried over.
    /// All sixteen occurrences in the wiki sit on pages whose paragraphs are
    /// perfectly intact, so refusing to touch text that already has line
    /// breaks would have repaired none of them.
    #[test]
    fn a_page_with_real_line_breaks_still_gets_its_quotes_back() {
        let body = concat!(
            "## Ozet\n\n",
            r#"Local models generated generic titles such as \"Session Summary\"."#,
            "\n\nFixed.",
        );
        let reply = json!({"title": "t", "body": body, "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert!(
            digest.body.contains(r#"such as "Session Summary"."#),
            "{:?}",
            digest.body
        );
        assert!(!digest.body.contains('\\'), "{:?}", digest.body);
    }

    /// Where an escaped quote is the point rather than a mistake. A page
    /// showing the assertion it just wrote would be corrupted by the repair.
    #[test]
    fn an_escaped_quote_inside_a_fence_is_left_alone() {
        let body = concat!(
            "The test reads:\n\n```rust\n",
            r#"assert_eq!(page, "a \"quoted\" name");"#,
            "\n```\n\nand it passes.",
        );
        let reply = json!({"title": "t", "body": body, "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert_eq!(digest.body, body, "code explaining an escape survives it");
    }

    /// The same line can hold both: an inline span that means it, and prose
    /// that does not.
    #[test]
    fn an_inline_span_keeps_its_escapes_and_the_prose_beside_it_does_not() {
        let body = concat!(
            r#"Redaction leaves `\"key\": \"[redacted]\"` where the secret was, "#,
            r#"so the page can be called \"safe\"."#,
        );
        let reply = json!({"title": "t", "body": body, "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert!(
            digest.body.contains(r#"`\"key\": \"[redacted]\"`"#),
            "the span was rewritten: {:?}",
            digest.body
        );
        assert!(
            digest.body.contains(r#"called "safe"."#),
            "the prose was not: {:?}",
            digest.body
        );
    }

    /// A backtick with nothing closing it opens no span, and the prose after
    /// it is still prose. Left unhandled this either loops or swallows the
    /// rest of the line.
    #[test]
    fn an_unclosed_backtick_does_not_protect_the_rest_of_the_line() {
        let body = r#"The flag is `--apply and the answer was \"no\"."#;
        let reply = json!({"title": "t", "body": body, "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert_eq!(
            digest.body,
            r#"The flag is `--apply and the answer was "no"."#
        );
    }

    #[test]
    fn a_reply_with_no_entities_is_still_a_page() {
        let reply = json!({"title": "t", "body": "b", "handoff": "h"});
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert!(digest.entities.is_empty());
    }

    #[test]
    fn the_schema_and_the_prompt_agree_about_entities() {
        assert!(schema()["properties"]["entities"].is_object());
        assert_eq!(
            schema()["required"],
            json!(["title", "body", "handoff", "entities", "notes"])
        );
        assert!(SYSTEM.contains("what a later search would type"));
        assert!(
            !SYSTEM.contains(char::from(92)),
            "a stray escape in the prompt reaches the model verbatim"
        );
    }

    /// The kinds are the enum the reply is constrained to and the namespaces
    /// the pages are filed under, and the two lists have to stay one list: a
    /// kind the schema offers and [`NoteKind::parse`] does not know is a note
    /// the model was invited to write and this code silently drops.
    #[test]
    fn the_schema_and_the_prompt_agree_about_notes() {
        let kinds = schema()["properties"]["notes"]["items"]["properties"]["kind"]["enum"].clone();
        let offered = kinds.as_array().expect("an enum of kinds");
        assert_eq!(offered.len(), 3);
        for kind in offered {
            let name = kind.as_str().expect("a kind name");
            let parsed = NoteKind::parse(name).expect("the schema offers a kind this code knows");
            assert_eq!(parsed.namespace(), format!("{name}s"));
        }
        assert!(SYSTEM.contains("Most sessions leave nothing behind"));
    }

    /// `_rules/` is the project's own voice. It outranks everything during
    /// retrieval, and nothing a model writes about one session belongs there.
    #[test]
    fn a_note_cannot_be_filed_in_the_namespace_the_project_speaks_in() {
        for kind in [NoteKind::Decision, NoteKind::Gotcha, NoteKind::Procedure] {
            assert_ne!(kind.namespace(), "_rules");
            assert!(
                anamnesis_core::page::AUTHORITY_NAMESPACES.contains(&kind.namespace()),
                "a note is filed where retrieval ranks it: {}",
                kind.namespace()
            );
        }
    }

    #[test]
    fn a_whitespace_only_field_is_rejected() {
        let reply = json!({"title": "   ", "body": "b", "handoff": "h"});
        assert!(digest_from_json(&reply, &session()).is_err());
    }

    #[test]
    fn the_prompt_names_the_files_and_failures_the_model_needs() {
        let prompt = render_prompt(
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
        );
        assert!(prompt.contains("add the llm provider"));
        assert!(prompt.contains("crates/anamnesis-llm/src/lib.rs"));
        assert!(prompt.contains("(FAILED)"));
        assert!(prompt.contains("Working directory"));
    }

    #[test]
    fn preferences_are_included_but_cannot_take_the_whole_budget() {
        let preferences = "ticket numbers matter. ".repeat(2_000);
        let prompt = render_prompt(
            &session(),
            &working_session(),
            Surroundings {
                preferences: Some(&preferences),
                ..Surroundings::default()
            },
            1_000,
        );
        assert!(prompt.contains("Project preferences"));
        assert!(prompt.contains("ticket numbers matter"));
        assert!(
            prompt.contains("add the llm provider"),
            "transcript survived"
        );
        assert!(estimate_tokens(&prompt) <= 1_100, "prompt stayed bounded");
    }

    /// The model cannot see the wiki, so the prompt is the only place a real
    /// path can come from. Without this the linking rule invites invention.
    #[test]
    fn the_pages_a_model_may_link_to_are_named_in_the_prompt() {
        let pages = vec![
            "gotchas/a-checkout-decided-what-a-database-could-open.md".to_owned(),
            "decisions/0001-storage.md".to_owned(),
        ];
        let prompt = render_prompt(
            &session(),
            &working_session(),
            Surroundings {
                pages: &pages,
                ..Surroundings::default()
            },
            6_500,
        );
        assert!(prompt.contains("Pages already in this memory"));
        assert!(prompt.contains("- decisions/0001-storage.md"));
        assert!(
            prompt.contains("add the llm provider"),
            "the transcript still gets the rest"
        );
    }

    /// A project with a thousand pages must not spend the session's budget on
    /// a directory listing — and what is left out is said, because a model told
    /// to link only to what it can see should know it is not seeing everything.
    #[test]
    fn a_long_list_of_pages_is_cut_to_whole_paths_and_says_so() {
        let pages: Vec<String> = (0..500).map(|i| format!("notes/page-{i:03}.md")).collect();
        let prompt = render_prompt(
            &session(),
            &working_session(),
            Surroundings {
                pages: &pages,
                ..Surroundings::default()
            },
            1_000,
        );

        assert!(prompt.contains("more pages, not listed"));
        assert!(estimate_tokens(&prompt) <= 1_100, "prompt stayed bounded");
        for line in prompt.lines().filter(|line| line.starts_with("- notes/")) {
            assert!(
                line.ends_with(".md"),
                "a clipped path is not a shorter path, it is a broken link: {line}"
            );
        }
        assert!(
            prompt.contains("add the llm provider"),
            "and the transcript is still there"
        );
    }

    /// A project whose wiki is empty gets no section at all, rather than an
    /// empty heading inviting links to nothing.
    #[test]
    fn an_empty_memory_is_not_offered_as_a_list() {
        let prompt = render_prompt(
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
        );
        assert!(!prompt.contains("Pages already in this memory"));
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

        let prompt = render_prompt(&session(), &observations, Surroundings::default(), 1_200);
        assert!(prompt.contains("FIRST PROMPT"));
        assert!(prompt.contains("LAST PROMPT"));
        assert!(prompt.contains("events omitted"));
        assert!(estimate_tokens(&prompt) <= 1_300);
    }

    #[test]
    fn a_transcript_that_fits_is_not_disturbed() {
        let prompt = render_prompt(
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
        );
        assert!(!prompt.contains("events omitted"));
    }

    #[test]
    fn newlines_in_a_prompt_do_not_break_the_one_line_per_event_shape() {
        let observations = [observation(
            EventKind::UserPrompt,
            "first line\nsecond line",
            None,
        )];
        let rendered = render_observation(&observations[0]);
        assert!(!rendered.contains('\n'));
        assert!(rendered.contains("second line"));
    }

    /// A provider that truncates once and then answers, recording what it was
    /// asked for each time.
    struct Cramped {
        asked: std::sync::Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl Provider for Cramped {
        fn name(&self) -> &'static str {
            "cramped"
        }
        fn model(&self) -> &str {
            "cramped-1"
        }
        async fn complete(&self, request: &Completion) -> Result<CompletionOutput, LlmError> {
            let mut asked = self.asked.lock().expect("lock");
            asked.push(request.schema.clone());
            if asked.len() == 1 {
                return Err(LlmError::Truncated("did not fit".to_owned()));
            }
            Ok(CompletionOutput {
                json: json!({
                    "title": "The long one",
                    "body": "## What. It was long.",
                    "handoff": "h",
                    "entities": [],
                }),
                model: "cramped-1".to_owned(),
                input_tokens: 1,
                output_tokens: 1,
            })
        }
    }

    /// The failure real use found, and what it costs now.
    ///
    /// The page and the durable pages share one output budget, so a long
    /// session whose model was generous with notes lost *everything* — the
    /// page became a tally of tool calls because of a field that was never
    /// required. Asking again identically cannot help; the budget has not
    /// moved. Dropping the optional half can.
    #[tokio::test]
    async fn a_reply_that_did_not_fit_is_asked_for_again_without_the_notes() {
        let provider = Cramped {
            asked: std::sync::Mutex::new(Vec::new()),
        };
        let (digest, source) = consolidate_with_source(
            &provider,
            &session(),
            &working_session(),
            Surroundings::default(),
            4_000,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(
            source,
            DigestSource::Model,
            "the page is not lost to a note"
        );
        assert_eq!(digest.title, "2026-08-20: The long one");
        assert!(digest.notes.is_empty());

        let asked = provider.asked.lock().expect("lock");
        assert_eq!(asked.len(), 2, "asked twice, not more");
        assert!(
            asked[0]["properties"]["notes"].is_object(),
            "the first ask is the full one"
        );
        assert!(
            asked[1]["properties"]["notes"].is_null(),
            "the second drops what was optional: {:?}",
            asked[1]["properties"]
        );
        assert_eq!(
            asked[1]["required"],
            json!(["title", "body", "handoff", "entities"]),
            "and does not go on requiring what it no longer offers"
        );
    }

    /// One note, written out in full, asserting the two things a note is: a
    /// namespace it is filed under and a name derived from what it claims.
    #[test]
    fn a_note_is_filed_by_its_kind_and_named_by_its_claim() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [],
            "notes": [{
                "kind": "gotcha",
                "title": "A moved crate breaks the Docker build",
                "body": "## What happens\n\nCargo verifies the move; the Dockerfile does not.",
            }],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        let [note] = &digest.notes[..] else {
            panic!("one note, got {:?}", digest.notes);
        };
        assert_eq!(note.kind, NoteKind::Gotcha);
        assert_eq!(
            note.path.as_str(),
            "gotchas/a-moved-crate-breaks-the-docker-build.md"
        );
        assert_eq!(note.title, "A moved crate breaks the Docker build");
        assert_eq!(note.kind.tier(), anamnesis_core::page::Tier::Procedural);
    }

    /// The normal reply. A session that taught the project nothing durable is
    /// the common case, and it has to be expressible without the absence
    /// looking like a malformed answer.
    #[test]
    fn a_session_that_leaves_nothing_durable_leaves_no_notes() {
        for notes in [json!([]), Value::Null] {
            let reply = json!({
                "title": "t", "body": "b", "handoff": "h",
                "entities": [], "notes": notes,
            });
            let digest = digest_from_json(&reply, &session()).expect("a digest");
            assert!(digest.notes.is_empty(), "{:?}", digest.notes);
        }
    }

    /// A reply with no `notes` field at all — an older provider, or a model
    /// that dropped a key it read as optional — is a reply with a page in it,
    /// and the page is the part worth keeping.
    #[test]
    fn a_reply_without_notes_is_still_a_page() {
        let reply = json!({"title": "t", "body": "b", "handoff": "h", "entities": []});
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert!(digest.notes.is_empty());
    }

    /// Each way a note can be unusable, and the thing none of them may do:
    /// take the session's page down with it.
    #[test]
    fn an_unusable_note_is_dropped_and_the_page_survives() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [],
            "notes": [
                {"kind": "philosophy", "title": "On memory", "body": "Real body."},
                {"kind": "decision", "title": "A real title", "body": "   "},
                {"kind": "decision", "title": "!!! ---", "body": "Real body."},
                {"kind": "gotcha", "title": "The one that survives", "body": "Real body."},
            ],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert_eq!(digest.body, "b", "the page is untouched by a bad note");
        let [note] = &digest.notes[..] else {
            panic!("one note, got {:?}", digest.notes);
        };
        assert_eq!(note.title, "The one that survives");
    }

    /// Not a spelling quibble: the whole batch is written in one commit, so
    /// the loser of a collision is overwritten in the same breath it was
    /// written in, and no history records that it ever existed.
    #[test]
    fn two_notes_that_would_be_the_same_file_are_one() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [],
            "notes": [
                {"kind": "gotcha", "title": "The server has an owner", "body": "First."},
                {"kind": "gotcha", "title": "The server has an owner!", "body": "Second."},
            ],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        assert_eq!(digest.notes.len(), 1);
        assert_eq!(digest.notes[0].body, "First.", "the first one written wins");
    }

    /// Same title, different namespace, is two pages. A decision and the
    /// gotcha it left behind may be named the same thing.
    #[test]
    fn the_same_claim_in_two_namespaces_is_two_notes() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [],
            "notes": [
                {"kind": "decision", "title": "Sessions close before the model runs", "body": "a"},
                {"kind": "gotcha", "title": "Sessions close before the model runs", "body": "b"},
            ],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert_eq!(digest.notes.len(), 2);
    }

    #[test]
    fn a_model_cannot_leave_more_notes_than_the_ceiling() {
        let many: Vec<Value> = (0..12)
            .map(|n| json!({"kind": "decision", "title": format!("decision {n}"), "body": "b"}))
            .collect();
        let reply = json!({
            "title": "t", "body": "b", "handoff": "h", "entities": [], "notes": many,
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert_eq!(digest.notes.len(), MAX_NOTES);
    }

    /// A plural kind is not a different answer. The schema's enum says
    /// `gotcha`; a model that writes the directory's name instead meant the
    /// same thing, and dropping the note would spend a real page on grammar.
    #[test]
    fn a_kind_written_in_the_plural_is_the_same_kind() {
        let reply = json!({
            "title": "t", "body": "b", "handoff": "h", "entities": [],
            "notes": [{"kind": "Gotchas", "title": "A real claim", "body": "Real body."}],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert_eq!(digest.notes[0].kind, NoteKind::Gotcha);
    }

    /// The session page's two title habits, one namespace down: a label in
    /// front of the claim, and a heading repeating it inside the body.
    #[test]
    fn a_note_does_not_restate_the_directory_it_is_filed_in() {
        let reply = json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
            "entities": [],
            "notes": [{
                "kind": "gotcha",
                "title": "Gotcha: The server has an owner",
                "body": "# The server has an owner\n\nA scheduled task starts it.",
            }],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        let note = &digest.notes[0];
        assert_eq!(note.title, "The server has an owner");
        assert_eq!(note.path.as_str(), "gotchas/the-server-has-an-owner.md");
        assert_eq!(note.body, "A scheduled task starts it.");
    }

    /// The escaping the handoff had to survive, arriving in a note instead.
    #[test]
    fn a_note_that_escaped_its_own_newlines_gets_them_back() {
        let reply = json!({
            "title": "t", "body": "b", "handoff": "h", "entities": [],
            "notes": [{
                "kind": "procedure",
                "title": "Rebasing a stack after a squash",
                "body": r"Close the PR.\n\nOpen a new one from the rebased branch.",
            }],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");

        let body = &digest.notes[0].body;
        assert!(!body.contains(r"\n"), "{body:?}");
        assert!(body.contains("the PR.\n\nOpen"), "{body:?}");
    }
}
