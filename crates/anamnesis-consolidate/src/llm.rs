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

use std::collections::{HashSet, VecDeque};

use anamnesis_core::observation::{EventKind, Observation, RESULT_MARKER, is_harness_prompt};
use anamnesis_core::page::{Entity, PagePath};
use anamnesis_core::session::Session;
use anamnesis_llm::{
    Completion, CompletionOutput, LlmError, Provider, clip_to_tokens, estimate_tokens,
};
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

/// How much of a tool's *result* one transcript line may carry.
///
/// Its own allowance rather than a share of the body, so that a long command
/// cannot spend the room its own output needed. Smaller than the command's,
/// because the end of a result is usually one line — `test result: ok`,
/// `error[E0308]`, an exit code — while a command can legitimately be a
/// paragraph of shell.
const MAX_RESULT_CHARS: usize = 200;

/// Completed actions retained as the session's deterministic resume point.
///
/// Four held the last verification and the follow-up inspection in the real
/// 873-event session that motivated this checkpoint, without turning a
/// handoff back into a transcript.
const MAX_RESUME_ACTIONS: usize = 4;

/// Bounded fields in the resume checkpoint. Keeping separate allowances means
/// a long command can never crowd out the result tail that says whether it
/// worked.
const MAX_RESUME_REQUEST_BYTES: usize = 320;
const MAX_RESUME_TOOL_BYTES: usize = 32;
const MAX_RESUME_INPUT_BYTES: usize = 64;
const MAX_RESUME_RESULT_BYTES: usize = 120;

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
failed and, if it is visible, why — quoting the error the tool printed rather \
than summarising it away.
- Say what was actually established, in the terms the session produced: the \
test that passed, the number that was measured, the error that was fixed, the \
file that ended up different. A page that says work was carried out, on a \
session with the verdict sitting in front of it, is a page written from the \
tool names alone.
- Name files and identifiers exactly as they appear.
- The title names this session, not its genre. Every page here is a session \
report, so `Session Summary`, `Session Handoff` and `Session Report` pick out \
none of them, and neither does an identifier copied out of a path. Do not \
begin the title with the word `Session`, and do not label it before saying \
it: write what this session was about, the way somebody scanning a directory \
listing would want it named.
- Write in the language the person wrote their prompts in, in that language's own alphabet. A page written in Turkish with the Turkish letters stripped out — `Ozet` for `Özet`, `gorev` for `görev` — is a page in no language at all, and it is also unsearchable by anybody typing the word properly.
- An `assistant-message` line is what the agent said when it finished a turn: its own account of what it had just done, written for a person. It is the most direct statement of intent and outcome in the transcript, and where it and the tool calls disagree, say what the tools show — an account written before a command failed is still what was believed at the time.
- A `harness-notification` line arrived through the prompt hook but was written by the agent harness, not the person. It is evidence about background work, never the person's request. The `Resume evidence` section already separates the last real request from this chatter.
- A `subagent-report` line is what a subagent handed back, labelled with the kind of agent that produced it. A subagent is a whole investigation inside one tool call, and its report is the only record of what it found: the calls it made are not in this transcript.
- A tool line shows what was run and, after a `→`, the end of what came back. Read it: that is where a command says whether it worked. `(FAILED)` marks a call the harness reported as failed, and `(NO RESULT ...)` marks one the agent started that never came back, which on some harnesses is the only trace a failed call leaves. Do not describe a session as having gone well because nothing is marked; the header says when this harness reports no outcomes at all, and then nothing being marked means nothing.
- The handoff is read by an agent that has no other context and a limited \
budget for it. Use the compact labels `Objective:`, `Verified:`, `Working \
state:`, `Next action:`, `Blockers:`, and `User constraints:` when the \
session provides those facts. Omit labels whose value is genuinely unknown. \
Say what to know and what to do next — not a chronological retelling. Never \
drop a concrete verdict or last action from `Resume evidence`; those facts are \
there because another agent needs the exact resume point.
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
- The clearest case for a note is something the person told the agent about \
the project for later: a rule to keep to, how or where it is deployed, a name \
or value no file in the repository holds. It clears the bar even when the \
session's own work was about something else, and then most of all, because a \
later session looking for it will not think to look in a page about that other \
work. Give it a note of its own, in the person's words — a decision for a \
rule, a procedure for a way of doing something — and still mention it on the \
session page.
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
                "description": "A compact operational brief for the next session, under 1500 characters. Use Objective, Verified, Working state, Next action, Blockers, and User constraints labels when known. Preserve exact verdicts and the last action from Resume evidence.",
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
    consolidate_attributed(
        provider,
        session,
        observations,
        surroundings,
        max_input_tokens,
        max_output_tokens,
    )
    .await
    .map(|attributed| (attributed.digest, attributed.source))
}

/// A digest, what produced it, and which model wrote it.
#[derive(Debug, Clone)]
pub struct Attributed {
    /// The page, handoff and notes.
    pub digest: SessionDigest,
    /// Whether a model wrote it or it was counted.
    pub source: DigestSource,
    /// The model that wrote it, when that was not the configured one.
    ///
    /// `None` both when the configured model wrote the page and when nobody
    /// did, so a caller recording provenance writes the configured model's
    /// name in either case — as it did before a chain could answer — and a
    /// stand-in's name only when there was one.
    pub stood_in: Option<String>,
}

impl Attributed {
    /// The model a caller should record against this digest.
    ///
    /// The stand-in that wrote it, if one did; otherwise the configured model,
    /// which either wrote it or is the model that did not answer.
    #[must_use]
    pub fn model<'a>(&'a self, provider: &'a dyn Provider) -> &'a str {
        self.stood_in.as_deref().unwrap_or_else(|| provider.model())
    }
}

/// The same again, saying which model wrote the page.
///
/// For callers that record provenance. A provider that is a chain can have a
/// reply written by a link other than the one it is named after, and the
/// session row is what `status` reads to say which model has been writing.
pub async fn consolidate_attributed(
    provider: &dyn Provider,
    session: &Session,
    observations: &[Observation],
    surroundings: Surroundings<'_>,
    max_input_tokens: usize,
    max_output_tokens: u32,
) -> Option<Attributed> {
    // The deterministic digest is computed first and unconditionally. It costs
    // microseconds, it decides whether this session is worth a page at all,
    // and holding it means the fallback below is a value rather than another
    // thing that can go wrong.
    let fallback = with_resume_checkpoint(consolidate(session, observations)?, observations);

    let (user, omitted) =
        render_prompt_reporting(session, observations, surroundings, max_input_tokens);
    let mut request = Completion {
        system: SYSTEM.to_owned(),
        user,
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
                let (digest, output) =
                    unless_garbled(provider, &request, session, digest, output).await;
                tracing::info!(
                    provider = provider.name(),
                    model = %output.model,
                    instead_of = output.instead_of.as_deref(),
                    input_tokens = output.input_tokens,
                    output_tokens = output.output_tokens,
                    "session consolidated by model"
                );
                let digest = disclosing(digest, observations.len(), omitted);
                // A model may accurately describe three hours of work and
                // still omit the final test verdict and the inspection that
                // followed it. Those are the two facts a replacement agent
                // needs most, so keep a small recorded appendix outside the
                // model's judgement. It is evidence, not an inferred plan.
                let digest = with_resume_checkpoint(digest, observations);
                let stood_in = output.instead_of.as_ref().map(|_| output.model.clone());
                Some(Attributed {
                    digest: naming_the_stand_in(digest, &output),
                    source: DigestSource::Model,
                    stood_in,
                })
            }
            Err(reason) => {
                tracing::warn!(%reason, "model reply was not a page; using the counted summary");
                Some(Attributed {
                    digest: fallback,
                    source: DigestSource::Counted,
                    stood_in: None,
                })
            }
        },
        Err(error) => {
            tracing::warn!(%error, "model unavailable; using the counted summary");
            Some(Attributed {
                digest: fallback,
                source: DigestSource::Counted,
                stood_in: None,
            })
        }
    }
}

/// Say, on the page, that a fallback wrote it.
///
/// On the page rather than only in the session row, for the reason
/// [`disclosing`] gives: the page is what is read a month later, by someone
/// weighing how much to trust it, and a smaller local model writing where a
/// hosted one was configured is exactly what that reader would want to know.
fn naming_the_stand_in(mut digest: SessionDigest, output: &CompletionOutput) -> SessionDigest {
    let Some(configured) = &output.instead_of else {
        return digest;
    };
    digest.body = format!(
        "{}\n\nWritten by {}, standing in for {configured}, which did not answer.\n",
        digest.body.trim_end(),
        output.model
    );
    digest
}

/// Fewest damaged letters that make a reply worth asking for again.
///
/// A floor, so that an English page quoting one name — `café`, `Gödel` — from a
/// session whose transcript happened to hold a letter beside it is not a
/// second request.
const GARBLED_AT_LEAST: usize = 3;

/// Share of a reply's non-ASCII letters that must be damaged, as a divisor.
///
/// One in ten. The two damaged sessions found had 31% and 27%; the pages of
/// fourteen sound ones in the same memory, eight recorded as the same model's,
/// had at most 1%, and that was two capital `İ`s.
const GARBLED_SHARE: usize = 10;

/// Ask for a reply again when its letters look damaged, and keep the better.
///
/// Found in this project's own memory, on two pages Gemini flash models wrote
/// eight days apart — one recorded as gemini-3.5-flash, the other recompiled
/// the afternoon the setup moved from 3.6-flash to 3.5-flash — in Turkish, from
/// transcripts whose Turkish was intact: not
/// one `ş`, `ğ`, `ü`, `ö` or `ç` on either page, and in their place `ő`, `đ`,
/// `œ`, a stray `w`, and once a C1 control character. `ı` came through
/// untouched on both. The prompts were rendered again and were sound, the same
/// code path had written gemini-3.5-flash's other Turkish pages cleanly, and
/// nothing downstream touches the letters — so the damage is in the reply, and it
/// arrives as valid JSON with every field filled. Every check this module makes
/// passed, and a page that no search for `şema` or `için` will ever find was
/// written as the model's.
///
/// Asked again rather than refused: a reply that trips this is almost always a
/// reply the model gets right on a second try, and one that trips it honestly —
/// some language nobody here reads — gets the same letters the second time and
/// is kept. The cost of being wrong is one request, never the page.
async fn unless_garbled(
    provider: &dyn Provider,
    request: &Completion,
    session: &Session,
    digest: SessionDigest,
    output: CompletionOutput,
) -> (SessionDigest, CompletionOutput) {
    // The material, not the instructions: the system prompt quotes `Özet` and
    // `görev` to every session, and letters it holds say nothing about what
    // this session's person typed.
    let shown = &request.user;
    let garbled = garbled_letters(&digest, shown);
    if garbled == 0 {
        return (digest, output);
    }
    tracing::warn!(
        garbled,
        "the reply wrote letters the session never used, beside ones it did; asking again"
    );
    let again = match provider.complete(request).await {
        Ok(again) => again,
        Err(error) => {
            tracing::warn!(%error, "asking again failed; keeping the first reply");
            return (digest, output);
        }
    };
    match digest_from_json(&again.json, session) {
        Ok(retry) if garbled_letters(&retry, shown) < garbled => (retry, again),
        Ok(_) => {
            tracing::warn!(
                garbled,
                "asked again and the letters were no better; keeping the first reply"
            );
            (digest, output)
        }
        Err(reason) => {
            tracing::warn!(%reason, "asked again and the reply was not a page; keeping the first");
            (digest, output)
        }
    }
}

/// How many letters in a reply look like damaged copies of the session's own.
///
/// Zero unless the reply as a whole looks damaged — see [`GARBLED_AT_LEAST`]
/// and [`GARBLED_SHARE`] — so that the number can be compared between two
/// replies and zero means leave it alone.
///
/// A letter counts when the model was never shown it, in either case, and it
/// shares its first UTF-8 byte with a letter the model was shown. That is the
/// shape both damaged pages had: `ş` (C5 9F) came back as `ő` (C5 91) on one
/// and `œ` (C5 93) on the other, `ğ` (C4 9F) as `đ` (C4 91) on both — the
/// first byte kept, the second one wrong. Requiring the first byte is what
/// leaves a correct reply alone when the person typed Turkish without its
/// letters and the model, as the system prompt asks, wrote them properly:
/// nothing in that transcript sits beside `ş`. Only two-byte letters are read,
/// since those are the ones a single wrong byte turns into another letter.
///
/// A C1 control character counts wherever it appears. No prose holds one, and
/// the second page had `ç` (C3 A7) come back as U+0087 (C2 87).
fn garbled_letters(digest: &SessionDigest, shown: &str) -> usize {
    fn two_byte(c: char) -> bool {
        ('\u{80}'..'\u{800}').contains(&c)
    }
    // The first byte of a two-byte UTF-8 sequence is `110` and the top five
    // of the character's eleven bits, so the bits above the low six name it.
    fn first_byte(c: char) -> u32 {
        u32::from(c) >> 6
    }

    let mut known: HashSet<char> = HashSet::new();
    for c in shown.chars().filter(|c| !c.is_ascii()) {
        known.insert(c);
        known.extend(c.to_lowercase());
        known.extend(c.to_uppercase());
    }
    let beside: HashSet<u32> = known
        .iter()
        .filter(|c| c.is_alphabetic() && two_byte(**c))
        .map(|c| first_byte(*c))
        .collect();

    let reply = std::iter::once(digest.title.as_str())
        .chain([digest.body.as_str(), digest.handoff.as_str()])
        .chain(
            digest
                .notes
                .iter()
                .flat_map(|note| [note.title.as_str(), note.body.as_str()]),
        );

    let (mut letters, mut garbled) = (0, 0);
    for c in reply.flat_map(str::chars).filter(|c| !c.is_ascii()) {
        if ('\u{80}'..'\u{a0}').contains(&c) {
            garbled += 1;
            continue;
        }
        if !c.is_alphabetic() {
            continue;
        }
        letters += 1;
        let seen = known.contains(&c)
            || c.to_lowercase().any(|v| known.contains(&v))
            || c.to_uppercase().any(|v| known.contains(&v));
        if !seen && two_byte(c) && beside.contains(&first_byte(c)) {
            garbled += 1;
        }
    }

    if garbled >= GARBLED_AT_LEAST && garbled * GARBLED_SHARE >= letters {
        garbled
    } else {
        0
    }
}

/// Facts kept outside the model's judgement because they define where work
/// stopped, rather than what the whole session was about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeEvidence {
    last_request: Option<String>,
    actions: Vec<String>,
}

impl ResumeEvidence {
    /// Read the latest real request and the last actions after it.
    ///
    /// An action is a completed call, or an attempt that never came back.
    /// Claude Code fires no post-tool hook for a call that failed, so when the
    /// last thing a session did was a test run that broke, the attempt is the
    /// only trace of it — and leaving it out would name the successful call
    /// before it as where work stopped.
    fn from_observations(observations: &[Observation]) -> Option<Self> {
        let last_request = observations
            .iter()
            .enumerate()
            .rev()
            .find(|(_, observation)| {
                observation.kind == EventKind::UserPrompt
                    && !is_harness_prompt(observation.body.as_str())
                    && !observation.body.as_str().trim().is_empty()
            });
        let after = last_request.map_or(0, |(index, _)| index + 1);

        let unfinished = crate::unfinished_attempts(observations);
        let mut actions = VecDeque::new();
        for (index, observation) in observations.iter().enumerate().skip(after) {
            let unanswered =
                observation.kind == EventKind::ToolAttempt && unfinished.contains(&index);
            if observation.kind != EventKind::ToolUse && !unanswered {
                continue;
            }
            let Some(action) = resume_action(observation) else {
                continue;
            };
            if actions.len() == MAX_RESUME_ACTIONS {
                actions.pop_front();
            }
            actions.push_back(action);
        }

        if actions.is_empty() {
            return None;
        }
        Some(Self {
            last_request: last_request.map(|(_, observation)| {
                clip_bytes(
                    &observation.body.as_str().trim().replace(['\n', '\r'], " "),
                    MAX_RESUME_REQUEST_BYTES,
                )
            }),
            actions: actions.into_iter().collect(),
        })
    }

    fn render_markdown(&self) -> String {
        let mut out = String::new();
        if let Some(request) = &self.last_request {
            out.push_str(&format!("- Last human request: {request}\n"));
        }
        for action in &self.actions {
            out.push_str(&format!("- Action: {action}\n"));
        }
        out
    }

    fn render_handoff(&self) -> String {
        let mut out = String::from("Recorded resume checkpoint:\n");
        if let Some(request) = &self.last_request {
            out.push_str(&format!("User constraint/request: {request}\n"));
        }
        for action in &self.actions {
            out.push_str(&format!("Action: {action}\n"));
        }
        out.trim_end().to_owned()
    }
}

/// Name one call by its useful input and the tail of its result, marked the
/// way the transcript marks it when it failed or never came back.
fn resume_action(observation: &Observation) -> Option<String> {
    let reference = observation.tool.as_ref()?;
    let mut tool = clip_bytes(reference.name.trim(), MAX_RESUME_TOOL_BYTES);
    if reference.ok == Some(false) {
        tool.push_str(" (FAILED)");
    } else if observation.kind == EventKind::ToolAttempt {
        tool.push_str(" (NO RESULT)");
    }
    let body = observation.body.as_str().trim();
    if body.is_empty() {
        return None;
    }
    let (input, result) = body
        .split_once(RESULT_MARKER)
        .map_or((body, None), |(input, result)| (input, Some(result)));
    let input = resume_input(input);
    let result = result
        .map(str::trim)
        .filter(|result| !result.is_empty())
        .map(|result| tail_clip_bytes(&result.replace(['\n', '\r'], " "), MAX_RESUME_RESULT_BYTES));

    let action = match (input.is_empty(), result) {
        (false, Some(result)) => format!("{tool}: {input} -> {result}"),
        (false, None) => format!("{tool}: {input}"),
        (true, Some(result)) => format!("{tool}: {result}"),
        (true, None) => return None,
    };
    Some(action)
}

/// Prefer a tool's human description, then the most recognisable argument.
fn resume_input(input: &str) -> String {
    let parsed = serde_json::from_str::<Value>(input).ok();
    let value = parsed
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| {
            ["description", "command", "file_path", "pattern"]
                .iter()
                .find_map(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_str)
                        .map(|value| (*key, value))
                })
        });
    let (kind, value) = value.unwrap_or(("", input));
    let value = value.trim().replace(['\n', '\r'], " ");
    if kind == "file_path" {
        tail_clip_bytes(&value, MAX_RESUME_INPUT_BYTES)
    } else {
        clip_bytes(&value, MAX_RESUME_INPUT_BYTES)
    }
}

/// Keep the end of a result, where command-line tools put their verdict.
fn tail_clip_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let room = max.saturating_sub('…'.len_utf8());
    let mut start = text.len().saturating_sub(room);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", text[start..].trim_start())
}

/// Repair both durable outputs with a bounded, recorded resume checkpoint.
fn with_resume_checkpoint(
    mut digest: SessionDigest,
    observations: &[Observation],
) -> SessionDigest {
    let Some(evidence) = ResumeEvidence::from_observations(observations) else {
        return digest;
    };

    digest.body = format!(
        "{}\n\n## Recorded resume checkpoint\n\n{}",
        digest.body.trim_end(),
        evidence.render_markdown().trim_end()
    );

    let checkpoint = evidence.render_handoff();
    let separator = "\n\n";
    let prose_budget = HANDOFF_LIMIT.saturating_sub(checkpoint.len() + separator.len());
    let prose = clip_bytes(digest.handoff.trim(), prose_budget);
    digest.handoff = if prose.is_empty() {
        clip_bytes(&checkpoint, HANDOFF_LIMIT)
    } else {
        format!("{prose}{separator}{checkpoint}")
    };
    digest
}

/// Render the material for one session, inside a token budget.
pub fn render_prompt(
    session: &Session,
    observations: &[Observation],
    surroundings: Surroundings<'_>,
    max_tokens: usize,
) -> String {
    render_prompt_reporting(session, observations, surroundings, max_tokens).0
}

/// The same, and how many events did not fit.
///
/// A separate entry point rather than a changed return type, because most
/// callers want the prompt and nothing else, and the two that want the number
/// want it for the same reason: an omission that only the model is told about
/// is an omission nobody can act on. The marker inside the prompt says it to
/// the model; this says it to us.
pub fn render_prompt_reporting(
    session: &Session,
    observations: &[Observation],
    surroundings: Surroundings<'_>,
    max_tokens: usize,
) -> (String, usize) {
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

    // The transcript marks failures and nothing else, so a harness that never
    // states an outcome produces a transcript indistinguishable from a session
    // where everything worked — and the model, told that failures are the most
    // useful thing in a transcript, faithfully reports a clean run. Saying it
    // once here is cheaper than annotating every line, and it is the only
    // place the model can learn that the absence means nothing.
    if crate::outcomes(observations).unreported() {
        out.push_str(
            "- Tool outcomes: not reported by this harness. No call below can be marked \
             FAILED, and the absence of failures is not evidence that none occurred — do not \
             report the session as succeeding on that basis.\n",
        );
    }

    if let Some(evidence) = ResumeEvidence::from_observations(observations) {
        out.push_str("\n# Resume evidence\n\n");
        out.push_str(
            "These are the recorded facts nearest the end of the person's latest request. \
             Treat them as the resume point; do not replace exact verdicts with a generic \
             statement that work was started.\n",
        );
        out.push_str(&evidence.render_markdown());
    }

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

    // An attempt whose completion arrived is the same call told twice, and a
    // transcript that tells every call twice is a transcript half as long. The
    // ones worth a line are those with no completion behind them: on a harness
    // that reports no failure, that absence is the failure.
    let unfinished = crate::unfinished_attempts(observations);
    let lines: Vec<String> = observations
        .iter()
        .enumerate()
        .filter(|(index, o)| o.kind != EventKind::ToolAttempt || unfinished.contains(index))
        .map(|(_, o)| render_observation(o))
        .collect();
    let (lines, omitted) = fit_lines(lines, remaining);
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }

    (out, omitted)
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

    let kind = if observation.kind == EventKind::UserPrompt
        && is_harness_prompt(observation.body.as_str())
    {
        "harness-notification"
    } else {
        observation.kind.as_str()
    };
    let mut line = format!("[{time}] {kind}");

    if let Some(tool) = &observation.tool {
        line.push_str(&format!(" {}", tool.name));
        // Only failures are annotated. Marking every success would spend a
        // third of the transcript's budget restating the default.
        if tool.ok == Some(false) {
            line.push_str(" (FAILED)");
        } else if observation.kind == EventKind::ToolAttempt {
            // It reached the transcript at all only because no completion
            // followed it, which every caller filters on before rendering.
            line.push_str(" (NO RESULT — this call was started and never came back)");
        }
    }

    let body = observation.body.as_str().trim();
    if !body.is_empty() {
        // A prompt and the agent's own account of a turn are prose written to
        // be read; a tool body is JSON. The two kinds that carry an argument
        // get the full allowance.
        let limit = if matches!(
            observation.kind,
            EventKind::UserPrompt | EventKind::AssistantMessage | EventKind::SubagentReport
        ) {
            MAX_BODY_CHARS
        } else {
            MAX_BODY_CHARS / 2
        };
        // The two halves of a tool body are clipped separately, because they
        // are not competing for the same budget: a long command would
        // otherwise consume the whole allowance and the result — the half that
        // says what happened — would be cut off every time it was worth
        // reading. What survives is the start of the command and the end of
        // its output, which is where a verdict is printed.
        let flattened = match observation.body.as_str().split_once(RESULT_MARKER) {
            Some((input, result)) => format!(
                "{}{RESULT_MARKER}{}",
                clip(input.trim(), limit),
                clip(result.trim(), MAX_RESULT_CHARS)
            ),
            None => clip(body, limit),
        };
        // Newlines would break the one-line-per-event shape the model is
        // reading, and the shape is what makes a long transcript legible.
        let flattened = flattened.replace(['\n', '\r'], " ⏎ ");
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
fn fit_lines(lines: Vec<String>, budget: usize) -> (Vec<String>, usize) {
    let cost = |line: &String| estimate_tokens(line) + 1;
    let total: usize = lines.iter().map(cost).sum();
    if total <= budget {
        return (lines, 0);
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
    (out, omitted)
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

/// Say, on the page, how much of the session the page was written from.
///
/// The transcript is squeezed to fit before the model is asked anything, and
/// until now the only party told was the model — the prompt carries a marker
/// and the page carries nothing. So a summary written from a seventh of an
/// afternoon read exactly like one written from all of it, and the difference
/// was invisible from every side: the page cannot say what it did not see, and
/// nobody reading it later has the transcript open beside them.
///
/// Written into the body rather than logged, because a log is a file nobody
/// opens and the page is the thing read a month later, when the question is
/// why it does not mention the afternoon's real work.
///
/// Nothing is added when nothing was dropped. A line saying "all of it" on
/// every page is a line people stop reading, and then the one time it says
/// something else it is not read either.
fn disclosing(mut digest: SessionDigest, recorded: usize, omitted: usize) -> SessionDigest {
    if omitted == 0 {
        return digest;
    }
    let seen = recorded.saturating_sub(omitted);
    digest.body = format!(
        "{}\n\nWritten from {seen} of this session's {recorded} recorded events; \
         {omitted} did not fit the model's context.\n",
        digest.body.trim_end()
    );
    digest
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
                    call_id: None,
                }),
            ),
            observation(
                EventKind::ToolUse,
                "cargo test",
                Some(ToolRef {
                    name: "Bash".to_owned(),
                    ok: Some(false),
                    call_id: None,
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
                    instead_of: None,
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

    /// Regression for the 873-event Claude session whose model-written page
    /// said only that expansion had started even though the final tool output
    /// said `46/46` and the next call inspected hard-coded session ids.
    #[tokio::test]
    async fn a_vague_model_reply_is_repaired_with_the_recorded_resume_point() {
        let observations = vec![
            observation(
                EventKind::UserPrompt,
                "increase the test count; quality matters more than cost",
                None,
            ),
            observation(
                EventKind::UserPrompt,
                "<task-notification> background run completed",
                None,
            ),
            observation(
                EventKind::ToolUse,
                &format!(
                    r#"{{"description":"Run the checks selftest"}}{RESULT_MARKER}46/46 cases behave as expected"#
                ),
                Some(ToolRef {
                    name: "PowerShell".to_owned(),
                    ok: None,
                    call_id: None,
                }),
            ),
            observation(
                EventKind::ToolUse,
                &format!(
                    r#"{{"pattern":"twelve|S08|S12"}}{RESULT_MARKER}scenario.toml:163:id = "S12""#
                ),
                Some(ToolRef {
                    name: "Grep".to_owned(),
                    ok: None,
                    call_id: None,
                }),
            ),
        ];
        let vague = json!({
            "title": "Longrun expansion",
            "body": "The scenario expansion was started.",
            "handoff": "Continue expanding the scenario.",
            "entities": ["longrun.py"],
        });

        let digest = consolidate_with_llm(
            &Fake(Ok(vague)),
            &session(),
            &observations,
            Surroundings::default(),
            6_500,
            2_000,
        )
        .await
        .expect("a digest");

        for output in [&digest.body, &digest.handoff] {
            assert!(
                output.contains("46/46 cases behave as expected"),
                "{output}"
            );
            assert!(output.contains("S12"), "{output}");
            assert!(
                output.contains("quality matters more than cost"),
                "{output}"
            );
            assert!(!output.contains("task-notification"), "{output}");
        }
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

    /// The rules a page's quality actually turned on, kept as assertions so
    /// that a reworded prompt cannot quietly drop one. Each is here because a
    /// page in this project's own wiki went wrong without it: summaries of
    /// Turkish sessions written in stripped ASCII, and bodies that named the
    /// tools a session ran while the verdicts sat unread in the transcript.
    #[test]
    fn the_rules_that_pages_went_wrong_without_are_in_the_prompt() {
        for rule in [
            "own alphabet",
            "quoting the error",
            "Say what was actually established",
            "NO RESULT",
            "subagent-report",
            "assistant-message",
            "harness-notification",
            "nothing being marked means nothing",
        ] {
            assert!(SYSTEM.contains(rule), "the prompt no longer says {rule:?}");
        }
    }

    #[test]
    fn resume_evidence_separates_the_person_from_harness_chatter() {
        let observations = vec![
            observation(EventKind::UserPrompt, "add ten probes", None),
            observation(
                EventKind::UserPrompt,
                "<task-notification> monitor finished",
                None,
            ),
            observation(
                EventKind::ToolUse,
                &format!(
                    r#"{{"description":"Run the checks selftest"}}{RESULT_MARKER}46/46 cases behave as expected"#
                ),
                Some(ToolRef {
                    name: "PowerShell".to_owned(),
                    ok: None,
                    call_id: None,
                }),
            ),
        ];

        let prompt = render_prompt(&session(), &observations, Surroundings::default(), 4_000);

        assert!(prompt.contains("# Resume evidence"), "{prompt}");
        assert!(
            prompt.contains("Last human request: add ten probes"),
            "{prompt}"
        );
        assert!(
            prompt.contains("46/46 cases behave as expected"),
            "{prompt}"
        );
        assert!(
            prompt.contains("harness-notification: <task-notification>"),
            "{prompt}"
        );
        assert!(!prompt.contains("Last human request: <task-notification>"));
    }

    /// Claude Code sends no completion for a call that failed. A session whose
    /// last move was a test run that broke would otherwise hand over the
    /// build before it as where work stopped.
    #[test]
    fn resume_evidence_names_the_last_call_even_when_it_never_came_back() {
        let bash = |id: &str, ok: Option<bool>| {
            Some(ToolRef {
                name: "Bash".to_owned(),
                ok,
                call_id: Some(id.to_owned()),
            })
        };
        let observations = vec![
            observation(EventKind::UserPrompt, "run the suite", None),
            observation(
                EventKind::ToolAttempt,
                r#"{"command":"cargo build"}"#,
                bash("a", None),
            ),
            observation(
                EventKind::ToolUse,
                &format!(r#"{{"command":"cargo build"}}{RESULT_MARKER}Finished"#),
                bash("a", None),
            ),
            observation(
                EventKind::ToolUse,
                &format!(r#"{{"command":"cargo fmt --check"}}{RESULT_MARKER}Diff in llm.rs"#),
                bash("b", Some(false)),
            ),
            observation(
                EventKind::ToolAttempt,
                r#"{"command":"cargo test --workspace"}"#,
                bash("c", None),
            ),
        ];

        let evidence = ResumeEvidence::from_observations(&observations).expect("resume evidence");

        assert_eq!(evidence.actions.len(), 3, "{:?}", evidence.actions);
        assert!(
            evidence.actions[1].starts_with("Bash (FAILED): cargo fmt --check"),
            "{:?}",
            evidence.actions
        );
        assert_eq!(
            evidence.actions[2], "Bash (NO RESULT): cargo test --workspace",
            "the call that never came back is the last action, and said to be one"
        );
        assert!(
            !evidence
                .actions
                .iter()
                .any(|a| a.contains("NO RESULT") && a.contains("cargo build")),
            "an attempt that was answered is not told again: {:?}",
            evidence.actions
        );
    }

    #[test]
    fn resume_evidence_keeps_the_filename_at_the_end_of_a_long_path() {
        let observations = vec![observation(
            EventKind::ToolUse,
            &format!(
                r#"{{"file_path":"C:\\Berke\\anamnesis-worktrees\\ten-probes\\crates\\anamnesis-evals\\src\\checks.py"}}{RESULT_MARKER}updated"#
            ),
            Some(ToolRef {
                name: "Edit".to_owned(),
                ok: Some(true),
                call_id: None,
            }),
        )];

        let evidence = ResumeEvidence::from_observations(&observations).expect("resume evidence");
        let rendered = evidence.render_markdown();

        assert!(rendered.contains("checks.py"), "{rendered}");
        assert!(rendered.contains("updated"), "{rendered}");
    }

    #[test]
    fn resume_tail_clipping_obeys_a_byte_budget_without_splitting_unicode() {
        let verdict = format!("{}SONUÇ", "başarılı ".repeat(40));

        let clipped = tail_clip_bytes(&verdict, 64);

        assert!(clipped.len() <= 64, "{} bytes: {clipped}", clipped.len());
        assert!(clipped.ends_with("SONUÇ"), "{clipped}");
    }

    /// A transcript that told every call twice would be half as long for the
    /// same money. An attempt earns its line only when no completion followed
    /// it — and then it is the most important line on the page, because on
    /// this harness that absence is the failure.
    #[test]
    fn only_the_attempts_that_never_came_back_reach_the_model() {
        let paired = |id: &str| {
            Some(ToolRef {
                name: "Bash".to_owned(),
                ok: None,
                call_id: Some(id.to_owned()),
            })
        };
        let observations = vec![
            observation(EventKind::ToolAttempt, "cargo build", paired("a")),
            observation(EventKind::ToolUse, "cargo build", paired("a")),
            observation(EventKind::ToolAttempt, "rm -rf /tmp/x", paired("b")),
        ];

        let prompt = render_prompt(&session(), &observations, Surroundings::default(), 4_000);

        let transcript = prompt
            .split_once("# Transcript")
            .map(|(_, transcript)| transcript)
            .expect("a transcript section");
        assert_eq!(
            transcript.matches("cargo build").count(),
            1,
            "the transcript tells the completed call once:\n{prompt}"
        );
        assert!(prompt.contains("rm -rf /tmp/x"), "{prompt}");
        assert!(prompt.contains("NO RESULT"), "{prompt}");
    }

    /// A tool body carries what was run and what came back, and the model
    /// needs both. Clipped as one string, a long command would eat the whole
    /// allowance and the result — which is the half that says what happened —
    /// would never survive the cut.
    #[test]
    fn a_long_command_cannot_crowd_out_its_own_result() {
        let body = format!(
            "{{\"command\":\"{}\"}}{RESULT_MARKER}test result: ok. 81 passed; 0 failed",
            "cargo test --workspace --all-features ".repeat(40)
        );
        let observation = observation(
            EventKind::ToolUse,
            &body,
            Some(ToolRef {
                name: "Bash".to_owned(),
                ok: None,
                call_id: None,
            }),
        );

        let line = render_observation(&observation);

        assert!(line.contains("test result: ok. 81 passed"), "{line}");
        assert!(line.contains("cargo test --workspace"), "{line}");
    }

    /// The transcript marks failures and nothing else, which reads as "all
    /// clear" when the harness marks nothing. The model is told once, in the
    /// header, so it does not write a page reporting a clean run it has no
    /// evidence for — and is told nothing when outcomes do arrive, since a
    /// caveat on every prompt is a caveat the model learns to skip.
    #[test]
    fn a_transcript_with_no_outcomes_says_so_to_the_model() {
        let silent = vec![
            observation(EventKind::UserPrompt, "add the llm provider", None),
            observation(
                EventKind::ToolUse,
                "cargo test",
                Some(ToolRef {
                    name: "Bash".to_owned(),
                    ok: None,
                    call_id: None,
                }),
            ),
        ];

        let prompt = render_prompt(&session(), &silent, Surroundings::default(), 6_500);
        assert!(prompt.contains("Tool outcomes: not reported"), "{prompt}");

        let reported = render_prompt(
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
        );
        assert!(
            !reported.contains("Tool outcomes: not reported"),
            "{reported}"
        );
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
                    call_id: None,
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

    /// A page written from part of a session says which part.
    ///
    /// The failure it closes: a summary of a seventh of an afternoon reads
    /// exactly like a summary of all of it, and nobody reading the page later
    /// has the transcript open beside them to notice.
    #[tokio::test]
    async fn a_page_written_from_part_of_a_session_says_so() {
        let mut observations = vec![observation(EventKind::UserPrompt, "FIRST", None)];
        for index in 0..200 {
            observations.push(observation(
                EventKind::ToolUse,
                &format!("event number {index} with enough padding to cost something"),
                Some(ToolRef {
                    name: "Read".to_owned(),
                    ok: Some(true),
                    call_id: None,
                }),
            ));
        }

        let (digest, source) = consolidate_with_source(
            &Fake(Ok(json!({
                "title": "The long one",
                "body": "## What. It went on for a while.",
                "handoff": "h",
                "entities": [],
            }))),
            &session(),
            &observations,
            Surroundings::default(),
            600,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(source, DigestSource::Model);
        assert!(
            digest.body.contains("did not fit the model's context"),
            "{:?}",
            digest.body
        );
        assert!(
            digest.body.contains(&format!(
                "of this session's {} recorded events",
                observations.len()
            )),
            "{:?}",
            digest.body
        );
    }

    /// And a page written from all of it says nothing, because a line that
    /// appears on every page is a line nobody reads on the one that matters.
    #[tokio::test]
    async fn a_page_written_from_the_whole_session_says_nothing_about_it() {
        let (digest, _) = consolidate_with_source(
            &Fake(Ok(json!({
                "title": "The short one",
                "body": "## What. It was brief.",
                "handoff": "h",
                "entities": [],
            }))),
            &session(),
            &working_session(),
            Surroundings::default(),
            6_500,
            1_000,
        )
        .await
        .expect("a digest");

        assert!(!digest.body.contains("did not fit"), "{:?}", digest.body);
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
                instead_of: None,
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

    /// A provider that gives its replies in order and repeats the last.
    struct Scripted {
        replies: Vec<Value>,
        asked: std::sync::Mutex<usize>,
    }

    impl Scripted {
        fn answering(replies: Vec<Value>) -> Self {
            Self {
                replies,
                asked: std::sync::Mutex::new(0),
            }
        }
        fn asked(&self) -> usize {
            *self.asked.lock().expect("lock")
        }
    }

    #[async_trait]
    impl Provider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn model(&self) -> &str {
            "scripted-1"
        }
        async fn complete(&self, _: &Completion) -> Result<CompletionOutput, LlmError> {
            let mut asked = self.asked.lock().expect("lock");
            let reply = self.replies[(*asked).min(self.replies.len() - 1)].clone();
            *asked += 1;
            Ok(CompletionOutput {
                json: reply,
                model: "scripted-1".to_owned(),
                input_tokens: 1,
                output_tokens: 1,
                instead_of: None,
            })
        }
    }

    /// A session whose person wrote Turkish with its letters.
    fn turkish_session() -> Vec<Observation> {
        vec![
            observation(EventKind::SessionStart, "", None),
            observation(
                EventKind::UserPrompt,
                "değişikliği geri al, testi düzelt ve çalıştır",
                None,
            ),
        ]
    }

    fn turkish_reply(body: &str) -> Value {
        json!({
            "title": "Geri alma",
            "body": body,
            "handoff": "Test çalışıyor.",
            "entities": [],
        })
    }

    const SOUND: &str = "Değişiklik geri alındı, test düzeltildi ve çalıştırıldı.";
    /// The damage as it was found: `ş` as `ő`, `ğ` as `đ`, `ü` gone to `w`.
    const DAMAGED: &str = "Deđiőiklik geri alındı, test dwzeltildi ve alıőtırıldı.";

    fn digest_with(body: &str) -> SessionDigest {
        digest_from_json(&turkish_reply(body), &session()).expect("a digest")
    }

    fn shown() -> &'static str {
        "değişikliği geri al, testi düzelt ve çalıştır"
    }

    /// The shape the two damaged pages had: letters the session never used,
    /// each sharing a first byte with one it did.
    #[test]
    fn letters_beside_the_sessions_own_are_counted() {
        assert_eq!(garbled_letters(&digest_with(DAMAGED), shown()), 3);
        assert_eq!(garbled_letters(&digest_with(SOUND), shown()), 0);
    }

    /// A capital the transcript only had in lower case is the same letter, and
    /// the sound pages this was measured on used them — `Ç`, `Ş`, `Ö` —
    /// wherever a sentence began with one.
    #[test]
    fn a_capital_of_a_letter_the_session_used_is_not_damage() {
        let digest = digest_with("Çalıştırıldı. Şimdi Üstteki Çalışıyor. Ğ yok.");
        assert_eq!(garbled_letters(&digest, shown()), 0);
    }

    /// The person typed Turkish without its letters and the model, as it is
    /// told to, wrote them. Nothing in the transcript sits beside `ş` or `ğ`,
    /// so none of it counts.
    #[test]
    fn letters_nobody_typed_are_not_damage_when_nothing_typed_sits_beside_them() {
        let typed_plainly = "degisikligi geri al, testi duzelt ve calistir";
        let digest = digest_with("Değişiklik ğğğ şşş geri alındı.");
        assert_eq!(garbled_letters(&digest, typed_plainly), 0);
    }

    /// One name from another language, on a page with plenty of its own
    /// letters, is below both bars.
    #[test]
    fn one_foreign_name_is_not_damage() {
        let digest = digest_with("Değişiklik geri alındı; ő bir isim. Test düzeltildi.");
        assert_eq!(garbled_letters(&digest, shown()), 0);
    }

    /// No prose holds a C1 control; the second damaged page had `ç` come back
    /// as one.
    #[test]
    fn c1_controls_count_wherever_they_are() {
        let digest = digest_with("Test d\u{87}zeltildi \u{87}al\u{87}\u{87}.");
        assert_eq!(garbled_letters(&digest, shown()), 4);
    }

    /// The durable pages come from the same reply and were damaged with it:
    /// the live case wrote a gotcha in the same broken letters.
    #[test]
    fn a_notes_letters_are_read_too() {
        let reply = json!({
            "title": "Geri alma",
            "body": SOUND,
            "handoff": "Test çalışıyor.",
            "entities": [],
            "notes": [{
                "kind": "gotcha",
                "title": "Canlı sorgular decay verisini bozar",
                "body": "Geliőtirme sırasında canlı veri wzerinde yapılan őeyler kalıcı ize dwőer.",
            }],
        });
        let digest = digest_from_json(&reply, &session()).expect("a digest");
        assert!(garbled_letters(&digest, shown()) >= GARBLED_AT_LEAST);
    }

    #[tokio::test]
    async fn a_damaged_reply_is_asked_for_again_and_the_sound_one_kept() {
        let provider = Scripted::answering(vec![turkish_reply(DAMAGED), turkish_reply(SOUND)]);
        let (digest, source) = consolidate_with_source(
            &provider,
            &session(),
            &turkish_session(),
            Surroundings::default(),
            6_500,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(provider.asked(), 2);
        assert_eq!(source, DigestSource::Model);
        assert_eq!(digest.body, SOUND);
    }

    /// Something that trips the check honestly trips it again, and the page
    /// is still the model's: the cost of a wrong guess is one request.
    #[tokio::test]
    async fn a_reply_damaged_twice_is_kept_rather_than_lost() {
        let provider = Scripted::answering(vec![turkish_reply(DAMAGED)]);
        let (digest, source) = consolidate_with_source(
            &provider,
            &session(),
            &turkish_session(),
            Surroundings::default(),
            6_500,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(provider.asked(), 2, "asked again once, not more");
        assert_eq!(source, DigestSource::Model);
        assert_eq!(digest.body, DAMAGED);
    }

    /// A chain whose configured model did not answer, and whose local
    /// fallback did.
    struct StoodIn;

    #[async_trait]
    impl Provider for StoodIn {
        fn name(&self) -> &'static str {
            "google"
        }
        fn model(&self) -> &str {
            "gemini-3.5-flash"
        }
        async fn complete(&self, _: &Completion) -> Result<CompletionOutput, LlmError> {
            Ok(CompletionOutput {
                json: good_reply(),
                model: "qwen2.5:7b-instruct".to_owned(),
                input_tokens: 1,
                output_tokens: 1,
                instead_of: Some("gemini-3.5-flash".to_owned()),
            })
        }
    }

    /// A page a stand-in wrote says so where it is read, and the session is
    /// recorded against the model that wrote it rather than the one the
    /// provider is named after.
    #[tokio::test]
    async fn a_page_a_fallback_wrote_names_it_and_is_attributed_to_it() {
        let attributed = consolidate_attributed(
            &StoodIn,
            &session(),
            &working_session(),
            Surroundings::default(),
            64_000,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(attributed.source, DigestSource::Model);
        assert_eq!(attributed.stood_in.as_deref(), Some("qwen2.5:7b-instruct"));
        assert_eq!(attributed.model(&StoodIn), "qwen2.5:7b-instruct");
        assert!(
            attributed.digest.body.ends_with(
                "Written by qwen2.5:7b-instruct, standing in for gemini-3.5-flash, \
                 which did not answer.\n"
            ),
            "{:?}",
            attributed.digest.body
        );
    }

    /// The ordinary page carries no such line, and is attributed as before.
    #[tokio::test]
    async fn a_page_the_configured_model_wrote_says_nothing_about_standing_in() {
        let provider = Fake(Ok(good_reply()));
        let attributed = consolidate_attributed(
            &provider,
            &session(),
            &working_session(),
            Surroundings::default(),
            64_000,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(attributed.stood_in, None);
        assert_eq!(attributed.model(&provider), "fake-1");
        assert!(!attributed.digest.body.contains("standing in"));
    }

    #[tokio::test]
    async fn a_sound_reply_is_asked_for_once() {
        let provider = Scripted::answering(vec![turkish_reply(SOUND)]);
        consolidate_with_source(
            &provider,
            &session(),
            &turkish_session(),
            Surroundings::default(),
            6_500,
            1_000,
        )
        .await
        .expect("a digest");

        assert_eq!(provider.asked(), 1);
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

    /// A reply exercising every field [`schema`] declares, so that removing
    /// any one of them has something to change.
    fn reply_using_every_declared_field() -> Value {
        json!({
            "title": "The provider was added",
            "body": "## What happened\n\nThe provider crate was written.",
            "handoff": "The provider exists; `cargo test` failed once and was not rerun.",
            "entities": ["anamnesis-llm", "openai.rs"],
            "notes": [{
                "kind": "gotcha",
                "title": "A refusal is not a transport failure",
                "body": "## Why\n\nRetrying one costs money and changes nothing.",
            }],
        })
    }

    /// **The #667 constraint, as a test rather than a comment.**
    ///
    /// ai-memory asked their model for typed relations in the prompt, had no
    /// field on the struct to hold them, and dropped every edge before the
    /// wiki write — silently, because a request that succeeds while losing
    /// what it asked for looks exactly like a request that succeeded.
    ///
    /// The same fault is available here and would be quieter: replies are read
    /// field by field with `value.get(name)` rather than deserialized into a
    /// struct, so nothing at all fails when the schema asks for something no
    /// reader reads. This asserts the property that would have caught it —
    /// **every field the schema declares must be load-bearing** — by removing
    /// each one in turn and requiring the outcome to change. A field nothing
    /// reads is a field whose absence nothing notices.
    #[test]
    fn every_field_the_schema_asks_for_changes_what_is_produced() {
        let full = reply_using_every_declared_field();
        let declared: Vec<String> = schema()["properties"]
            .as_object()
            .expect("the schema declares properties")
            .keys()
            .cloned()
            .collect();
        assert!(
            declared.len() >= 5,
            "the schema stopped declaring the fields this test was written against: {declared:?}"
        );

        let with_everything = digest_from_json(&full, &session())
            .expect("a reply using every declared field is valid");

        for name in &declared {
            let mut without = full.clone();
            without
                .as_object_mut()
                .expect("the reply is an object")
                .remove(name);

            // Either the field is required and its absence is refused, or it
            // is optional and its absence shows. Both are "something noticed".
            match digest_from_json(&without, &session()) {
                Err(_) => {}
                Ok(reduced) => assert_ne!(
                    reduced, with_everything,
                    "the schema asks the model for {name:?} and nothing reads it: dropping it \
                     produced an identical digest. Either give it a reader, or stop asking."
                ),
            }
        }
    }

    /// The reduced schema is the same promise one field shorter, so it has to
    /// keep the same property — a request that trades `notes` away to fit must
    /// not quietly trade anything else.
    #[test]
    fn the_reduced_schema_asks_for_nothing_it_does_not_read_either() {
        let declared: Vec<String> = schema_without_notes()["properties"]
            .as_object()
            .expect("the reduced schema declares properties")
            .keys()
            .cloned()
            .collect();
        assert!(!declared.contains(&"notes".to_owned()));

        let mut full = reply_using_every_declared_field();
        full.as_object_mut().expect("object").remove("notes");
        let with_everything =
            digest_from_json(&full, &session()).expect("valid without the notes field");

        for name in &declared {
            let mut without = full.clone();
            without.as_object_mut().expect("object").remove(name);
            match digest_from_json(&without, &session()) {
                Err(_) => {}
                Ok(reduced) => assert_ne!(
                    reduced, with_everything,
                    "the reduced schema asks for {name:?} and nothing reads it"
                ),
            }
        }
    }

    /// The other half of #667, and the half a removal test cannot reach: the
    /// reduced schema must stay a *subset* of the full one. A field spelled
    /// one way in `schema` and another in `schema_without_notes` is a request
    /// the model answers and the reader looks for under a name it never sent.
    #[test]
    fn the_reduced_schema_is_the_full_one_minus_exactly_notes() {
        let full = schema();
        let reduced = schema_without_notes();

        let mut expected = full["properties"].clone();
        expected
            .as_object_mut()
            .expect("object")
            .remove("notes")
            .expect("the full schema declares notes");
        assert_eq!(reduced["properties"], expected);

        let required: Vec<&str> = full["required"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| *name != "notes")
            .collect();
        assert_eq!(
            reduced["required"]
                .as_array()
                .expect("array")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            required
        );
        assert_eq!(
            reduced["additionalProperties"],
            full["additionalProperties"]
        );
    }
}
