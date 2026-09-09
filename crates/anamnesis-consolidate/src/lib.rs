//! Turning a session's observations into a page and a handoff.
//!
//! This is the deterministic path: no model, no network, no API key. It exists
//! for two reasons. It is the fallback when no LLM is configured, and it is the
//! thing an LLM summary is measured against — if a generated summary is not
//! clearly better than "here is what happened, counted", the prompt needs work
//! rather than the model needing to be larger.
//!
//! What it cannot do is judge *why* something was done. That is the part worth
//! spending a model on.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;

use anamnesis_core::observation::{EventKind, Observation};
use anamnesis_core::page::{Entity, PagePath, Tier};
use anamnesis_core::session::Session;

mod files;
mod llm;

pub use files::{changed_files, mentioned_files};
pub use llm::{
    DigestSource, PREFERENCES_PAGE, Surroundings, consolidate_with_llm, consolidate_with_source,
    render_prompt, render_prompt_reporting, schema,
};

/// Longest handoff this module will produce, in bytes.
///
/// Deliberately far below the observation budget: a handoff is injected into
/// the next session's context, where every byte competes with the work itself.
pub const HANDOFF_LIMIT: usize = 2_048;

/// How many user prompts to quote in the page body.
const MAX_QUOTED_PROMPTS: usize = 8;

/// How many files to name before summarising the rest as a count.
const MAX_NAMED_FILES: usize = 12;

/// Longest single quoted prompt, in characters.
const MAX_PROMPT_CHARS: usize = 400;

/// Longest quoted closing message, in characters.
///
/// Longer than a prompt's allowance and deliberately so: a prompt is a request
/// and its first sentence is the request, while a closing message is the
/// account — what was done, what came of it, what is still open — and its last
/// paragraph is usually the part a later session needs.
const MAX_ACCOUNT_CHARS: usize = 900;

/// How many closing messages a page quotes.
///
/// The last ones, not the first: a session's early turns are superseded by its
/// later ones, and the final account is the one that knows how the work ended.
const MAX_QUOTED_ACCOUNTS: usize = 3;

/// How many entities a page may name.
///
/// The same ceiling `memory_write_page` puts on an agent writing a page by
/// hand. Past a handful the inverse-frequency weighting is doing all the work
/// anyway, and a page that claims to be about twenty things is about none.
pub const MAX_ENTITIES: usize = 10;

/// Most durable pages one session may leave behind.
///
/// Deliberately small. These are filed in the namespaces that outrank ordinary
/// pages during retrieval, so each one is a standing claim on the top of every
/// later search — the cost of a weak one is not that it is ignored, it is that
/// it displaces something better. A session that reports five durable lessons
/// has almost always reported none, and is summarising itself twice: once as
/// its page and once as a list.
pub const MAX_NOTES: usize = 3;

/// What kind of durable page a note is, which is also where it is filed.
///
/// Three kinds rather than a free-form namespace, because the namespace
/// decides retrieval rank and a model naming its own would eventually reach
/// for `_rules/`. These three are the ones this wiki already ranks as
/// authority, minus `_rules/`, which is the project's own voice and not
/// something a summary of one session gets to add to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// A choice that was made, and what it was made against.
    Decision,
    /// Something that behaves differently than it looks like it does.
    Gotcha,
    /// A sequence worth following again.
    Procedure,
}

impl NoteKind {
    /// The wiki namespace pages of this kind are filed under.
    pub fn namespace(self) -> &'static str {
        match self {
            Self::Decision => "decisions",
            Self::Gotcha => "gotchas",
            Self::Procedure => "procedures",
        }
    }

    /// The tier a page of this kind is written at.
    ///
    /// Following what this wiki already does rather than inventing a mapping:
    /// a decision is distilled durable knowledge, and both a gotcha and a
    /// procedure are a pattern named because it recurs. The tier is what keeps
    /// a sweep from treating these as one session's leftovers.
    pub fn tier(self) -> Tier {
        match self {
            Self::Decision => Tier::Semantic,
            Self::Gotcha | Self::Procedure => Tier::Procedural,
        }
    }

    /// Recover a kind from the word a model used for it.
    ///
    /// Singular or plural, in any case: the model is given an enum and mostly
    /// honours it, but a reply that says `gotchas` is not a reply that meant
    /// something else.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().trim_end_matches('s') {
            "decision" => Some(Self::Decision),
            "gotcha" => Some(Self::Gotcha),
            "procedure" => Some(Self::Procedure),
            _ => None,
        }
    }
}

/// A durable page a session left behind, beyond its own account of itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Which kind of page this is, deciding its namespace and its tier.
    pub kind: NoteKind,
    /// Where the page goes.
    ///
    /// Derived from the title during validation rather than carried from the
    /// model or worked out at write time: a title that cannot be made into a
    /// path is a note that cannot be written, and a digest that has already
    /// been accepted is the wrong place to discover that.
    pub path: PagePath,
    /// The claim the page makes.
    pub title: String,
    /// Markdown body.
    pub body: String,
}

/// The result of consolidating one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDigest {
    /// Title for the session page.
    pub title: String,
    /// Markdown body for the session page.
    pub body: String,
    /// Bounded summary for the next session.
    pub handoff: String,
    /// Canonical names the page is about, for the entity retrieval stream.
    ///
    /// Empty is allowed and means exactly that: nothing nameable was found.
    /// A session page with no entities is still reachable through full text,
    /// links, and vectors — this stream is the one that finds a page whose
    /// words a searcher never used.
    pub entities: Vec<Entity>,
    /// Durable pages this session left behind, beyond its own.
    ///
    /// Empty is the normal case, and the case the prompt argues for: most
    /// sessions add to what a project has done without teaching it anything
    /// that outlives them. A wiki whose authority namespaces fill up at one
    /// page per session is one where being ranked highest stops meaning
    /// anything.
    pub notes: Vec<Note>,
}

/// Consolidate a session into a page and a handoff.
///
/// Returns `None` when the session carries nothing but lifecycle boundaries.
/// An agent that started and immediately stopped should not leave a page behind
/// — a wiki full of empty session stubs makes every later search worse.
pub fn consolidate(session: &Session, observations: &[Observation]) -> Option<SessionDigest> {
    let substantive = observations
        .iter()
        .filter(|o| !o.kind.is_boundary_only())
        .count();
    if substantive == 0 {
        return None;
    }

    let prompts = collect_prompts(observations);
    let accounts = collect_accounts(observations);
    let tools = count_tools(observations);
    let outcomes = outcomes(observations);
    let files = files::mentioned_files(observations);
    let changed = files::changed_files(observations);
    let truncated = observations
        .iter()
        .filter(|o| o.body.is_truncated())
        .count();

    // What a session changed is what a later search is looking for, so it gets
    // the entity slots first and the files it merely read get what is left.
    let entities = entities_from_files(&changed, &files);
    let title = title_for(session, prompts.first().map(String::as_str));
    let counted = Counted {
        prompts: &prompts,
        accounts: &accounts,
        tools: &tools,
        outcomes,
        changed: &changed,
        files: &files,
        truncated,
        total: observations.len(),
    };
    let body = render_body(session, &counted);
    let handoff = render_handoff(
        session, &prompts, &accounts, &tools, outcomes, &changed, &files,
    );

    Some(SessionDigest {
        title,
        body,
        handoff,
        entities,
        // Never any. Counting can say which tools ran and which failed; it
        // cannot tell a decision from a tool call, and a durable page is
        // exactly the judgement this path is defined by not making.
        notes: Vec::new(),
    })
}

/// Name the files a session worked on, as things the page is about.
///
/// Basenames, not paths. An entity matches when every token of its name is in
/// the query, so `crates/anamnesis-llm/src/lib.rs` would demand that a
/// searcher type all six of its tokens, while `lib.rs` asks for the two
/// someone would actually write. A basename that names half the wiki costs
/// nothing either: entity weighting is inverse to how many pages carry the
/// name, so `lib.rs` fades on its own while `docker-compose.yml` stays sharp.
///
/// This is what counting can reach. A model, when one is configured, names
/// the ideas instead — see `llm::consolidate_with_llm`.
fn entities_from_files(changed: &[String], mentioned: &[String]) -> Vec<Entity> {
    let mut names: Vec<String> = Vec::new();
    for file in changed.iter().chain(mentioned.iter()) {
        let base = file.rsplit('/').next().unwrap_or(file);
        if !base.is_empty() && !names.iter().any(|seen| seen == base) {
            names.push(base.to_owned());
        }
    }
    names.truncate(MAX_ENTITIES);
    names
        .iter()
        .filter_map(|name| Entity::parse(name).ok())
        .collect()
}

/// What the agent itself said, in order.
///
/// The counted page has always been written from the outside: which tools ran,
/// which files were named, how many of each. This is the one part of a session
/// that says what happened in words, and it costs nothing to quote — the agent
/// wrote it for a person to read, at the moment it knew most about the turn.
fn collect_accounts(observations: &[Observation]) -> Vec<String> {
    observations
        .iter()
        .filter(|o| o.kind == EventKind::AssistantMessage)
        .map(|o| clip(o.body.as_str().trim(), MAX_ACCOUNT_CHARS))
        .filter(|text| !text.is_empty())
        .collect()
}

/// Prompts written by the operator, in order.
fn collect_prompts(observations: &[Observation]) -> Vec<String> {
    observations
        .iter()
        .filter(|o| o.kind == EventKind::UserPrompt)
        .map(|o| clip(o.body.as_str().trim(), MAX_PROMPT_CHARS))
        .filter(|p| !p.is_empty())
        .collect()
}

/// Tool invocation counts, keyed by tool name.
///
/// Completions only. A harness with a pre-tool hook reports the same call
/// twice — once as an attempt, once as a completion — and counting both would
/// double every number on the page the day that hook was registered, which
/// would look exactly like the agent having worked twice as hard. Attempts
/// that never completed are counted separately, by [`outcomes`], because they
/// are a different fact.
fn count_tools(observations: &[Observation]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for observation in observations {
        if observation.kind != EventKind::ToolUse {
            continue;
        }
        if let Some(tool) = &observation.tool {
            *counts.entry(tool.name.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// What the harness said about how the session's tool calls went.
///
/// Three numbers rather than one, because a page that prints only failures
/// cannot tell the two silences apart: a session where every call succeeded
/// and a session where the harness never says whether anything succeeded both
/// render as nothing at all, and a reader takes nothing at all for "clean
/// run". `ToolRef::ok` is deliberately an `Option` for exactly this reason —
/// `None` means "not reported", not "fine" — and until now that distinction
/// died here, one function away from the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Outcomes {
    /// Tool calls recorded in this session.
    pub calls: usize,
    /// Of those, how many arrived with an outcome the harness stated.
    pub reported: usize,
    /// Of those, how many the harness stated had failed.
    pub failures: usize,
    /// Calls the agent began that never reported a result.
    ///
    /// Evidence rather than inference, and the only kind available on a
    /// harness that stays silent about failures: an attempt was recorded, the
    /// completion that always follows never arrived. On Claude Code that is
    /// precisely what a failed tool call looks like from outside.
    pub unfinished: usize,
}

impl Outcomes {
    /// Whether the harness said nothing about any of the calls it recorded.
    ///
    /// The condition for the line that replaces silence. Requires at least one
    /// call: a session that ran no tools has no outcomes to report and nobody
    /// needs telling that none were reported.
    pub fn unreported(&self) -> bool {
        self.calls > 0 && self.reported == 0
    }
}

/// The attempts that never completed, by position in `observations`.
///
/// Match each attempt against the completion that should have followed it.
///
/// By the harness's own identifier where there is one. Where there is not —
/// most harnesses send none — attempts and completions are matched by tool
/// name in order, so a session that attempted `Bash` four times and completed
/// it three has one unfinished call and not four. Anything left over is a call
/// that began and never came back.
pub(crate) fn unfinished_attempts(observations: &[Observation]) -> Vec<usize> {
    let mut completed_ids: Vec<&str> = Vec::new();
    let mut completed_names: BTreeMap<&str, usize> = BTreeMap::new();
    for observation in observations {
        if observation.kind != EventKind::ToolUse {
            continue;
        }
        if let Some(tool) = &observation.tool {
            match tool.call_id.as_deref() {
                Some(id) => completed_ids.push(id),
                None => *completed_names.entry(tool.name.as_str()).or_insert(0) += 1,
            }
        }
    }

    let mut unfinished = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        if observation.kind != EventKind::ToolAttempt {
            continue;
        }
        let Some(tool) = &observation.tool else {
            continue;
        };
        // An identified attempt is answered only by its own completion. This
        // is the exact case, and the one where a claim of failure is safe to
        // make.
        if let Some(id) = tool.call_id.as_deref() {
            if let Some(at) = completed_ids.iter().position(|seen| *seen == id) {
                completed_ids.swap_remove(at);
            } else {
                unfinished.push(index);
            }
            continue;
        }
        // Otherwise the best available answer: some completion of this tool.
        match completed_names.get_mut(tool.name.as_str()) {
            Some(left) if *left > 0 => *left -= 1,
            _ => unfinished.push(index),
        }
    }
    unfinished
}

/// Count the tool calls and what the harness said about them.
fn outcomes(observations: &[Observation]) -> Outcomes {
    let mut counted = Outcomes {
        unfinished: unfinished_attempts(observations).len(),
        ..Outcomes::default()
    };
    for tool in observations
        .iter()
        .filter(|o| o.kind == EventKind::ToolUse)
        .filter_map(|o| o.tool.as_ref())
    {
        counted.calls += 1;
        match tool.ok {
            Some(true) => counted.reported += 1,
            Some(false) => {
                counted.reported += 1;
                counted.failures += 1;
            }
            None => {}
        }
    }
    counted
}

/// A title derived from the first prompt, falling back to the date.
fn title_for(session: &Session, first_prompt: Option<&str>) -> String {
    let date = session.started_at.to_string();
    let date = date.split('T').next().unwrap_or(&date).to_owned();
    match first_prompt {
        Some(prompt) if !prompt.is_empty() => {
            let line = prompt.lines().next().unwrap_or(prompt).trim();
            format!("{date}: {}", clip(line, 60))
        }
        _ => format!("{date}: {} session", session.agent),
    }
}

/// Everything counted about one session, in the shape the page is written
/// from.
///
/// A struct rather than eight parameters, and the eight were not arbitrary:
/// each was added when the page learned to say something it could not say
/// before. Grouping them keeps the next such addition from being a decision
/// about argument order.
struct Counted<'a> {
    prompts: &'a [String],
    /// What the agent said when it finished each turn.
    accounts: &'a [String],
    tools: &'a BTreeMap<String, usize>,
    outcomes: Outcomes,
    /// Files a writing tool changed.
    changed: &'a [String],
    /// Files named anywhere in the session, changed or not.
    files: &'a [String],
    /// Observations whose body was cut to fit its budget.
    truncated: usize,
    /// Observations recorded, of every kind.
    total: usize,
}

/// The session page body.
fn render_body(session: &Session, counted: &Counted<'_>) -> String {
    let Counted {
        prompts,
        accounts,
        tools,
        outcomes,
        changed,
        files,
        truncated,
        total,
    } = *counted;
    let mut out = String::new();

    out.push_str("## Session\n\n");
    out.push_str(&format!("- Agent: {}\n", session.agent));
    out.push_str(&format!("- Started: {}\n", session.started_at));
    if let Some(ended) = session.ended_at {
        out.push_str(&format!("- Ended: {ended}\n"));
    }
    out.push_str(&format!("- Observations: {total}\n"));
    if truncated > 0 {
        out.push_str(&format!(
            "- Truncated bodies: {truncated} (content was cut to fit its budget)\n"
        ));
    }

    if !prompts.is_empty() {
        out.push_str("\n## Asked\n\n");
        for prompt in prompts.iter().take(MAX_QUOTED_PROMPTS) {
            out.push_str(&format!("- {}\n", prompt.replace('\n', " ")));
        }
        if prompts.len() > MAX_QUOTED_PROMPTS {
            out.push_str(&format!(
                "- ...and {} more\n",
                prompts.len() - MAX_QUOTED_PROMPTS
            ));
        }
    }

    // The agent's own words, and the only part of a counted page that is not
    // a tally. Placed above the counts because it is what a reader wants
    // first: a paragraph saying what happened beats a list of how often each
    // tool ran, and until this arrived the counted path had no way to produce
    // one. The last turns rather than the first — a session's early answers
    // are superseded by its later ones.
    if !accounts.is_empty() {
        out.push_str("\n## What the agent said it did\n\n");
        let skipped = accounts.len().saturating_sub(MAX_QUOTED_ACCOUNTS);
        if skipped > 0 {
            out.push_str(&format!(
                "_The last {MAX_QUOTED_ACCOUNTS} of {} turns._\n\n",
                accounts.len()
            ));
        }
        for account in accounts.iter().skip(skipped) {
            out.push_str(&format!("> {}\n\n", account.replace('\n', "\n> ")));
        }
    }

    if !changed.is_empty() {
        out.push_str("\n## Files changed\n\n");
        for file in changed.iter().take(MAX_NAMED_FILES) {
            out.push_str(&format!("- `{file}`\n"));
        }
        if changed.len() > MAX_NAMED_FILES {
            out.push_str(&format!(
                "- ...and {} more\n",
                changed.len() - MAX_NAMED_FILES
            ));
        }
    }

    // Only the files not already named above. One appearing under both
    // headings would make the first heading mean nothing.
    let mentioned: Vec<&String> = files.iter().filter(|f| !changed.contains(f)).collect();
    if !mentioned.is_empty() {
        out.push_str("\n## Files mentioned\n\n");
        for file in mentioned.iter().take(MAX_NAMED_FILES) {
            out.push_str(&format!("- `{file}`\n"));
        }
        if mentioned.len() > MAX_NAMED_FILES {
            out.push_str(&format!(
                "- ...and {} more\n",
                mentioned.len() - MAX_NAMED_FILES
            ));
        }
    }

    if !tools.is_empty() || outcomes.unfinished > 0 {
        out.push_str("\n## Tools\n\n");
        for (name, count) in sorted_by_count(tools) {
            out.push_str(&format!("- {name}: {count}\n"));
        }
        // Worth more than the line below it: this is a failure the harness
        // never named, recovered from the fact that a call it announced never
        // came back.
        if outcomes.unfinished > 0 {
            out.push_str(&format!(
                "- Calls that never reported back: {} (the agent began them and no result \
                 followed, which is what a failed call looks like on a harness that reports \
                 none)\n",
                outcomes.unfinished
            ));
        }
        // Said in full, because the page is read by someone who has no
        // transcript beside them. "Reported failures: 0" and no line at all
        // both claim a clean run; only one of the three states below is that
        // claim, and it is the one that has to be earned.
        if outcomes.unreported() {
            out.push_str(
                "- Reported failures: unknown — this harness reported no outcome for any of \
                 its tool calls, so a failure would not appear here\n",
            );
        } else {
            if outcomes.failures > 0 {
                out.push_str(&format!("- Reported failures: {}\n", outcomes.failures));
            }
            if outcomes.reported < outcomes.calls {
                out.push_str(&format!(
                    "- Outcomes reported for {} of {} tool calls; the rest are unknown\n",
                    outcomes.reported, outcomes.calls
                ));
            }
        }
    }

    out.push_str(
        "\n---\n\nCompiled without a model. Counts are observed; intent is not inferred.\n",
    );
    out
}

/// Openers that mark a prompt as the harness talking, not a person.
///
/// A hook records `UserPromptSubmit` as it arrives, and a harness submits
/// through the same door it gives a person: a background task finishing
/// reaches the session as a prompt like any other. In this project's own
/// index that is 18 of 71 recorded prompts, one in four.
///
/// They belong on the session page — they are part of what happened. They do
/// not belong on the one line of the handoff that says what was last asked
/// for, which is how a session that ended on `doğrulayalım` handed the next
/// one `Last request: <task-notification> <task-id>b1yxanpb2</task-id>`.
///
/// Only what has actually been seen is listed. A harness that injects
/// something else adds a line here, after somebody has looked at a real one.
const HARNESS_PROMPT_OPENERS: &[&str] = &["<task-notification>"];

/// Whether the harness generated this prompt rather than a person.
///
/// Matched at the start only. A person quoting a notification in the middle
/// of a question is asking a question.
fn is_harness_generated(prompt: &str) -> bool {
    let start = prompt.trim_start();
    HARNESS_PROMPT_OPENERS
        .iter()
        .any(|opener| start.starts_with(opener))
}

/// The last thing a person asked for, if a person asked for anything.
fn last_human_prompt(prompts: &[String]) -> Option<&String> {
    prompts.iter().rev().find(|p| !is_harness_generated(p))
}

/// The bounded note handed to the next session.
fn render_handoff(
    session: &Session,
    prompts: &[String],
    accounts: &[String],
    tools: &BTreeMap<String, usize>,
    outcomes: Outcomes,
    changed: &[String],
    files: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Previous session ({}, started {}).\n",
        session.agent, session.started_at
    ));

    if let Some(last) = last_human_prompt(prompts) {
        out.push_str(&format!(
            "Last request: {}\n",
            clip(&last.replace('\n', " "), 240)
        ));
    }

    // The agent's closing word, clipped hard. The next session is starting
    // with nothing, and one sentence of "here is where this was left" is worth
    // more to it than any count — but it is spent out of that session's own
    // context, so it gets a quarter of the budget and no more.
    if let Some(last) = accounts.last() {
        out.push_str(&format!(
            "Last account: {}\n",
            clip(&last.replace('\n', " "), 400)
        ));
    }

    // What was changed, whenever anything was. The next session needs to know
    // which files are no longer what it would otherwise find, and a list of
    // everything that was opened buries that under everything else.
    let (label, listed) = if changed.is_empty() {
        ("Files in play", files)
    } else {
        ("Files changed", changed)
    };
    if !listed.is_empty() {
        let named: Vec<&str> = listed.iter().take(6).map(String::as_str).collect();
        out.push_str(&format!("{label}: {}", named.join(", ")));
        if listed.len() > named.len() {
            out.push_str(&format!(" (+{} more)", listed.len() - named.len()));
        }
        out.push('\n');
    }

    if !tools.is_empty() {
        let summary: Vec<String> = sorted_by_count(tools)
            .into_iter()
            .take(4)
            .map(|(name, count)| format!("{name}×{count}"))
            .collect();
        out.push_str(&format!("Activity: {}", summary.join(", ")));
        // Short, because every byte here is spent out of the next session's
        // context — but present, because the next session otherwise starts by
        // believing nothing went wrong.
        if outcomes.failures > 0 {
            out.push_str(&format!(" ({} reported failures)", outcomes.failures));
        } else if outcomes.unfinished > 0 {
            out.push_str(&format!(
                " ({} calls never reported back)",
                outcomes.unfinished
            ));
        } else if outcomes.unreported() {
            out.push_str(" (no tool outcomes reported)");
        }
        out.push('\n');
    }

    clip_bytes(&out, HANDOFF_LIMIT)
}

/// Tool counts, most used first, ties broken by name for stable output.
fn sorted_by_count(tools: &BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut entries: Vec<(String, usize)> = tools.iter().map(|(k, v)| (k.clone(), *v)).collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries
}

/// Shorten to `max` characters, marking the cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Shorten to `max` bytes on a character boundary.
pub(crate) fn clip_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::{ObservationId, ProjectId, SessionId, WorkspaceId};
    use anamnesis_core::observation::{BoundedBody, ToolRef};
    use anamnesis_core::session::{AgentKind, SessionState};
    use jiff::Timestamp;

    fn session() -> Session {
        Session {
            id: SessionId::new(),
            agent: AgentKind::ClaudeCode,
            workspace_id: WorkspaceId::from_uuid(uuid::Uuid::nil()),
            project_id: ProjectId::from_uuid(uuid::Uuid::nil()),
            workstream_id: None,
            checkout_path: "/repo".into(),
            started_at: "2026-08-19T09:00:00Z".parse().unwrap(),
            ended_at: Some("2026-08-19T10:30:00Z".parse().unwrap()),
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
            at: Timestamp::now(),
            body: BoundedBody::truncating(body, BoundedBody::DEFAULT_LIMIT),
            sanitized: true,
        }
    }

    fn tool(name: &str, ok: Option<bool>) -> Option<ToolRef> {
        Some(ToolRef {
            name: name.to_owned(),
            ok,
            call_id: None,
        })
    }

    /// A call the harness named, so an attempt and its completion can be
    /// paired exactly rather than by tool name.
    fn identified(name: &str, call_id: &str) -> Option<ToolRef> {
        Some(ToolRef {
            name: name.to_owned(),
            ok: None,
            call_id: Some(call_id.to_owned()),
        })
    }

    #[test]
    fn the_files_a_session_touched_become_the_names_it_is_about() {
        let digest = consolidate(
            &session(),
            &[
                observation(EventKind::UserPrompt, "wire up the provider", None),
                observation(
                    EventKind::ToolUse,
                    "edited crates/anamnesis-llm/src/lib.rs and docker-compose.yml",
                    tool("Edit", Some(true)),
                ),
            ],
        )
        .expect("a digest");

        let named: Vec<&str> = digest.entities.iter().map(Entity::as_str).collect();
        assert!(named.contains(&"lib.rs"), "got {named:?}");
        assert!(named.contains(&"docker-compose.yml"), "got {named:?}");
        assert!(
            !named.iter().any(|name| name.contains('/')),
            "basenames, not paths: a path would demand every one of its tokens"
        );
    }

    #[test]
    fn one_file_named_twice_is_one_entity() {
        let digest = consolidate(
            &session(),
            &[
                observation(EventKind::UserPrompt, "fix the pipeline", None),
                observation(
                    EventKind::ToolUse,
                    "read crates/web/src/pipeline.rs",
                    tool("Read", Some(true)),
                ),
                observation(
                    EventKind::ToolUse,
                    "edited crates/web/src/pipeline.rs again",
                    tool("Edit", Some(true)),
                ),
            ],
        )
        .expect("a digest");

        assert_eq!(
            digest
                .entities
                .iter()
                .filter(|e| e.as_str() == "pipeline.rs")
                .count(),
            1
        );
    }

    #[test]
    fn a_session_that_touched_no_files_names_nothing() {
        let digest = consolidate(
            &session(),
            &[observation(
                EventKind::UserPrompt,
                "what did we decide?",
                None,
            )],
        )
        .expect("a digest");
        assert!(digest.entities.is_empty());
    }

    #[test]
    fn a_session_cannot_claim_to_be_about_everything() {
        let mut observations = vec![observation(EventKind::UserPrompt, "big refactor", None)];
        for n in 0..40 {
            observations.push(observation(
                EventKind::ToolUse,
                &format!("edited crates/thing/src/module{n}.rs"),
                tool("Edit", Some(true)),
            ));
        }

        let digest = consolidate(&session(), &observations).expect("a digest");
        assert_eq!(digest.entities.len(), MAX_ENTITIES);
    }

    #[test]
    fn a_session_with_only_boundaries_produces_no_page() {
        let observations = vec![
            observation(EventKind::SessionStart, "", None),
            observation(EventKind::SessionEnd, "", None),
        ];
        assert!(consolidate(&session(), &observations).is_none());
    }

    #[test]
    fn a_working_session_produces_a_page_and_a_handoff() {
        let observations = vec![
            observation(EventKind::SessionStart, "", None),
            observation(EventKind::UserPrompt, "add the storage layer", None),
            observation(
                EventKind::ToolUse,
                "edited crates/store/src/lib.rs",
                tool("Edit", Some(true)),
            ),
            observation(EventKind::ToolUse, "cargo test", tool("Bash", Some(false))),
            observation(EventKind::SessionEnd, "", None),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");
        assert!(digest.title.contains("add the storage layer"));
        assert!(digest.body.contains("Edit: 1"));
        assert!(digest.body.contains("Reported failures: 1"));
        assert!(digest.handoff.contains("add the storage layer"));
        assert!(digest.handoff.contains("claude-code"));
    }

    /// The failure this closes, as it actually arrived: this project's own
    /// harness reports no `tool_response` at all, so every session it recorded
    /// was consolidated with `ok == None` on every call — and the page, which
    /// prints a failure line only when there are failures, said nothing. A
    /// reader takes nothing for "clean run". The page cannot know whether
    /// anything failed; what it can do is stop implying it knows.
    #[test]
    fn a_harness_that_reports_no_outcome_is_not_a_session_without_failures() {
        let observations = vec![
            observation(EventKind::UserPrompt, "run the tests", None),
            observation(EventKind::ToolUse, "cargo test", tool("Bash", None)),
            observation(EventKind::ToolUse, "cargo clippy", tool("Bash", None)),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(
            digest.body.contains("Reported failures: unknown"),
            "{}",
            digest.body
        );
        assert!(
            digest.handoff.contains("no tool outcomes reported"),
            "{}",
            digest.handoff
        );
    }

    /// The counted path has always written from the outside — which tools
    /// ran, how many times. The agent's own closing message is the one part of
    /// a session that says what happened in words, and quoting it costs
    /// nothing: it was written for a person, at the moment the agent knew most
    /// about the turn.
    #[test]
    fn the_page_quotes_what_the_agent_said_it_did() {
        let observations = vec![
            observation(EventKind::UserPrompt, "fix the parser", None),
            observation(EventKind::ToolUse, "cargo test", tool("Bash", None)),
            observation(
                EventKind::AssistantMessage,
                "Parser fixed: the drive letter survives now, and `cargo test` passes 81 of 81.",
                None,
            ),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(
            digest.body.contains("## What the agent said it did"),
            "{}",
            digest.body
        );
        assert!(digest.body.contains("passes 81 of 81"), "{}", digest.body);
        assert!(
            digest.handoff.contains("Last account: Parser fixed"),
            "{}",
            digest.handoff
        );
    }

    /// A long session says several things and the last one knows how the work
    /// ended, so the page keeps the final turns rather than the opening ones —
    /// and says how many it left out, since a quote that silently stands for
    /// twelve is a quote nobody can weigh.
    #[test]
    fn the_last_turns_are_the_ones_quoted() {
        let mut observations = vec![observation(EventKind::UserPrompt, "go", None)];
        for turn in 1..=6 {
            observations.push(observation(
                EventKind::AssistantMessage,
                &format!("account number {turn}"),
                None,
            ));
        }

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(digest.body.contains("account number 6"), "{}", digest.body);
        assert!(digest.body.contains("account number 4"), "{}", digest.body);
        assert!(!digest.body.contains("account number 3"), "{}", digest.body);
        assert!(
            digest.body.contains("The last 3 of 6 turns"),
            "{}",
            digest.body
        );
    }

    /// A page that lists everything a session opened says it was about
    /// everything it opened. What it changed is the part a later search wants,
    /// so it gets its own heading, the first entity slots, and the line in the
    /// handoff.
    #[test]
    fn a_page_separates_what_was_changed_from_what_was_only_read() {
        let observations = vec![
            observation(EventKind::UserPrompt, "fix the parser", None),
            observation(
                EventKind::ToolUse,
                r#"{"file_path": "crates/anamnesis-hooks/src/lib.rs"}"#,
                tool("Read", None),
            ),
            observation(
                EventKind::ToolUse,
                r#"{"file_path": "crates/anamnesis-core/src/observation.rs"}"#,
                tool("Edit", None),
            ),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        let changed_at = digest
            .body
            .find("## Files changed")
            .expect("a changed heading");
        let mentioned_at = digest
            .body
            .find("## Files mentioned")
            .expect("a mentioned heading");
        assert!(changed_at < mentioned_at, "{}", digest.body);

        let changed_section = &digest.body[changed_at..mentioned_at];
        assert!(
            changed_section.contains("observation.rs"),
            "{changed_section}"
        );
        assert!(!changed_section.contains("hooks.rs"), "{changed_section}");

        // A file cannot be under both headings: the first would stop meaning
        // anything.
        let mentioned_section = &digest.body[mentioned_at..];
        assert!(
            !mentioned_section.contains("observation.rs"),
            "{mentioned_section}"
        );

        assert!(
            digest
                .handoff
                .contains("Files changed: crates/anamnesis-core/src/observation.rs"),
            "{}",
            digest.handoff
        );
        assert_eq!(
            digest.entities.first().map(|e| e.as_str()),
            Some("observation.rs"),
            "what changed takes the first entity slot"
        );
    }

    /// A session that changed nothing says so by saying nothing: the heading
    /// is absent and the handoff falls back to the files that were in play.
    #[test]
    fn a_session_that_changed_nothing_claims_nothing() {
        let observations = vec![
            observation(EventKind::UserPrompt, "explain the parser", None),
            observation(
                EventKind::ToolUse,
                r#"{"file_path": "crates/anamnesis-hooks/src/lib.rs"}"#,
                tool("Read", None),
            ),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(!digest.body.contains("## Files changed"), "{}", digest.body);
        assert!(
            digest.handoff.contains("Files in play"),
            "{}",
            digest.handoff
        );
    }

    /// The failure a silent harness leaves behind, recovered from its shape:
    /// the agent announced three calls and two came back. Claude Code fires no
    /// post-tool hook for a call that failed, so the missing completion is the
    /// only evidence the failure ever happened.
    #[test]
    fn an_attempt_with_no_completion_is_reported_as_a_call_that_failed() {
        let observations = vec![
            observation(EventKind::UserPrompt, "run the tests", None),
            observation(
                EventKind::ToolAttempt,
                "cargo test",
                identified("Bash", "a"),
            ),
            observation(EventKind::ToolUse, "cargo test", identified("Bash", "a")),
            observation(EventKind::ToolAttempt, "exit 3", identified("Bash", "b")),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(
            digest.body.contains("Calls that never reported back: 1"),
            "{}",
            digest.body
        );
        // The completed call is counted once, not twice: the attempt and the
        // completion are one call seen from two moments.
        assert!(digest.body.contains("Bash: 1"), "{}", digest.body);
        assert!(
            digest.handoff.contains("1 calls never reported back"),
            "{}",
            digest.handoff
        );
    }

    /// Without identifiers — which is what most harnesses send — attempts and
    /// completions are paired by tool name in order. Three attempts and two
    /// completions is one unfinished call, not three.
    #[test]
    fn attempts_pair_with_completions_by_name_when_nothing_identifies_them() {
        let mut observations = vec![observation(EventKind::UserPrompt, "build", None)];
        for _ in 0..3 {
            observations.push(observation(
                EventKind::ToolAttempt,
                "cargo build",
                tool("Bash", None),
            ));
        }
        for _ in 0..2 {
            observations.push(observation(
                EventKind::ToolUse,
                "cargo build",
                tool("Bash", None),
            ));
        }

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(
            digest.body.contains("Calls that never reported back: 1"),
            "{}",
            digest.body
        );
    }

    /// A session where every attempt came back says nothing about attempts at
    /// all. The line is evidence of a failure, and printing it at zero would
    /// make it noise on every page instead.
    #[test]
    fn a_session_where_every_call_returned_reports_no_unfinished_calls() {
        let observations = vec![
            observation(EventKind::ToolAttempt, "ls", identified("Bash", "a")),
            observation(EventKind::ToolUse, "ls", identified("Bash", "a")),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(
            !digest.body.contains("never reported back"),
            "{}",
            digest.body
        );
    }

    /// The other half of the same honesty: a session the harness *did* report
    /// on says nothing about unknowns, because there are none. A warning
    /// printed on every page is a warning nobody reads on the one page where
    /// it matters.
    #[test]
    fn a_reported_clean_run_carries_no_warning() {
        let observations = vec![
            observation(EventKind::UserPrompt, "run the tests", None),
            observation(EventKind::ToolUse, "cargo test", tool("Bash", Some(true))),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(!digest.body.contains("unknown"), "{}", digest.body);
        assert!(
            !digest.handoff.contains("no tool outcomes reported"),
            "{}",
            digest.handoff
        );
    }

    /// A harness that reports some calls and not others is the mixed case, and
    /// the page says how far its failure count reaches rather than letting the
    /// count stand for all of them.
    #[test]
    fn a_partly_reporting_harness_says_how_far_the_count_reaches() {
        let observations = vec![
            observation(EventKind::UserPrompt, "run the tests", None),
            observation(EventKind::ToolUse, "cargo test", tool("Bash", Some(false))),
            observation(EventKind::ToolUse, "cargo build", tool("Bash", None)),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");

        assert!(
            digest.body.contains("Reported failures: 1"),
            "{}",
            digest.body
        );
        assert!(
            digest
                .body
                .contains("Outcomes reported for 1 of 2 tool calls"),
            "{}",
            digest.body
        );
    }

    /// The counted path leaves no durable pages, on a session that plainly did
    /// something worth remembering. Counting can say which tools ran and which
    /// failed; whether a project learned a decision from that is the judgement
    /// this path is defined by not making, and guessing it would put pages
    /// that outrank everything into a wiki on the strength of a tool tally.
    #[test]
    fn counting_never_claims_a_session_left_something_durable() {
        let observations = vec![
            observation(
                EventKind::UserPrompt,
                "why does the docker build fail",
                None,
            ),
            observation(
                EventKind::ToolUse,
                "moved crates/evals to crates/anamnesis-evals",
                tool("Bash", Some(true)),
            ),
            observation(
                EventKind::ToolUse,
                "docker build .",
                tool("Bash", Some(false)),
            ),
        ];

        let digest = consolidate(&session(), &observations).expect("digest");
        assert!(digest.notes.is_empty());
    }

    #[test]
    fn the_handoff_stays_within_its_budget() {
        let long = "x".repeat(5_000);
        let mut observations = vec![observation(EventKind::UserPrompt, &long, None)];
        for index in 0..200 {
            observations.push(observation(
                EventKind::ToolUse,
                &format!("touched file-{index}.rs"),
                tool(&format!("Tool{index}"), Some(true)),
            ));
        }

        let digest = consolidate(&session(), &observations).expect("digest");
        assert!(
            digest.handoff.len() <= HANDOFF_LIMIT,
            "handoff was {} bytes",
            digest.handoff.len()
        );
    }

    #[test]
    fn multibyte_prompts_survive_clipping() {
        let prompt = "şükrü ".repeat(200);
        let observations = vec![observation(EventKind::UserPrompt, &prompt, None)];
        let digest = consolidate(&session(), &observations).expect("digest");
        // The assertion that matters is that clipping produced valid UTF-8 at
        // all; a byte-level cut through `ş` would have panicked before this.
        assert!(digest.handoff.contains('ş'));
    }

    #[test]
    fn tools_are_reported_most_used_first() {
        let mut observations = vec![observation(EventKind::UserPrompt, "go", None)];
        for _ in 0..3 {
            observations.push(observation(
                EventKind::ToolUse,
                "x",
                tool("Read", Some(true)),
            ));
        }
        for _ in 0..7 {
            observations.push(observation(
                EventKind::ToolUse,
                "x",
                tool("Edit", Some(true)),
            ));
        }

        let digest = consolidate(&session(), &observations).expect("digest");
        let edit = digest.body.find("Edit: 7").expect("edit listed");
        let read = digest.body.find("Read: 3").expect("read listed");
        assert!(edit < read, "the busier tool should come first");
    }

    #[test]
    fn truncated_observations_are_disclosed() {
        let huge = "y".repeat(BoundedBody::DEFAULT_LIMIT + 100);
        let observations = vec![observation(EventKind::UserPrompt, &huge, None)];
        let digest = consolidate(&session(), &observations).expect("digest");
        assert!(digest.body.contains("Truncated bodies: 1"));
    }

    #[test]
    fn output_is_stable_for_identical_input() {
        let observations = vec![
            observation(EventKind::UserPrompt, "do the thing", None),
            observation(EventKind::ToolUse, "src/a.rs", tool("Edit", Some(true))),
        ];
        let first = consolidate(&session(), &observations).expect("digest");
        let second = consolidate(&session(), &observations).expect("digest");
        assert_eq!(first, second);
    }

    #[test]
    fn a_session_without_prompts_still_gets_a_title() {
        let observations = vec![observation(
            EventKind::ToolUse,
            "x",
            tool("Bash", Some(true)),
        )];
        let digest = consolidate(&session(), &observations).expect("digest");
        assert!(digest.title.contains("2026-08-19"));
        assert!(digest.title.contains("claude-code"));
    }

    /// The failure this exists for, as it actually arrived: a session that
    /// ended on a real question, handed on as a background-task notice.
    #[test]
    fn a_task_notification_is_not_the_last_request() {
        let prompts = vec![
            "nerede kalmıştık".to_owned(),
            "doğrulayalım".to_owned(),
            "<task-notification> <task-id>b1yxanpb2</task-id> <status>completed</status>"
                .to_owned(),
        ];

        let chosen = last_human_prompt(&prompts).expect("a person asked something");

        assert_eq!(chosen, "doğrulayalım");
    }

    #[test]
    fn a_session_of_nothing_but_notifications_claims_no_request() {
        let prompts = vec![
            "<task-notification> one".to_owned(),
            "   <task-notification> two".to_owned(),
        ];

        assert!(last_human_prompt(&prompts).is_none());
    }

    /// Matched at the start only. Somebody asking about a notification is
    /// asking, and their question is the last request.
    #[test]
    fn quoting_a_notification_is_still_a_question() {
        let prompts = vec!["what does <task-notification> mean when the id is missing".to_owned()];

        assert_eq!(
            last_human_prompt(&prompts).map(String::as_str),
            Some("what does <task-notification> mean when the id is missing")
        );
    }

    #[test]
    fn the_handoff_line_names_the_person_not_the_harness() {
        let session = session();
        let prompts = vec![
            "fix the release".to_owned(),
            "<task-notification> done".to_owned(),
        ];
        let tools = BTreeMap::new();

        let handoff = render_handoff(
            &session,
            &prompts,
            &[],
            &tools,
            Outcomes::default(),
            &[],
            &[],
        );

        assert!(
            handoff.contains("Last request: fix the release"),
            "{handoff}"
        );
        assert!(!handoff.contains("task-notification"), "{handoff}");
    }
}
