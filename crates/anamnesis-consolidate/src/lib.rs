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
use anamnesis_core::page::Entity;
use anamnesis_core::session::Session;

mod files;
mod llm;

pub use files::mentioned_files;
pub use llm::{
    DigestSource, PREFERENCES_PAGE, consolidate_with_llm, consolidate_with_source, render_prompt,
    schema,
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

/// How many entities a page may name.
///
/// The same ceiling `memory_write_page` puts on an agent writing a page by
/// hand. Past a handful the inverse-frequency weighting is doing all the work
/// anyway, and a page that claims to be about twenty things is about none.
pub const MAX_ENTITIES: usize = 10;

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
    let tools = count_tools(observations);
    let failures = count_failures(observations);
    let files = files::mentioned_files(observations);
    let truncated = observations
        .iter()
        .filter(|o| o.body.is_truncated())
        .count();

    let entities = entities_from_files(&files);
    let title = title_for(session, prompts.first().map(String::as_str));
    let body = render_body(
        session,
        &prompts,
        &tools,
        failures,
        &files,
        truncated,
        observations.len(),
    );
    let handoff = render_handoff(session, &prompts, &tools, failures, &files);

    Some(SessionDigest {
        title,
        body,
        handoff,
        entities,
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
fn entities_from_files(files: &[String]) -> Vec<Entity> {
    let mut names: Vec<String> = Vec::new();
    for file in files {
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
fn count_tools(observations: &[Observation]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for observation in observations {
        if let Some(tool) = &observation.tool {
            *counts.entry(tool.name.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// Tool calls the harness reported as failed.
fn count_failures(observations: &[Observation]) -> usize {
    observations
        .iter()
        .filter(|o| o.tool.as_ref().is_some_and(|t| t.ok == Some(false)))
        .count()
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

/// The session page body.
fn render_body(
    session: &Session,
    prompts: &[String],
    tools: &BTreeMap<String, usize>,
    failures: usize,
    files: &[String],
    truncated: usize,
    total: usize,
) -> String {
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

    if !files.is_empty() {
        out.push_str("\n## Files mentioned\n\n");
        for file in files.iter().take(MAX_NAMED_FILES) {
            out.push_str(&format!("- `{file}`\n"));
        }
        if files.len() > MAX_NAMED_FILES {
            out.push_str(&format!(
                "- ...and {} more\n",
                files.len() - MAX_NAMED_FILES
            ));
        }
    }

    if !tools.is_empty() {
        out.push_str("\n## Tools\n\n");
        for (name, count) in sorted_by_count(tools) {
            out.push_str(&format!("- {name}: {count}\n"));
        }
        if failures > 0 {
            out.push_str(&format!("- Reported failures: {failures}\n"));
        }
    }

    out.push_str(
        "\n---\n\nCompiled without a model. Counts are observed; intent is not inferred.\n",
    );
    out
}

/// The bounded note handed to the next session.
fn render_handoff(
    session: &Session,
    prompts: &[String],
    tools: &BTreeMap<String, usize>,
    failures: usize,
    files: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Previous session ({}, started {}).\n",
        session.agent, session.started_at
    ));

    if let Some(last) = prompts.last() {
        out.push_str(&format!(
            "Last request: {}\n",
            clip(&last.replace('\n', " "), 240)
        ));
    }

    if !files.is_empty() {
        let named: Vec<&str> = files.iter().take(6).map(String::as_str).collect();
        out.push_str(&format!("Files in play: {}", named.join(", ")));
        if files.len() > named.len() {
            out.push_str(&format!(" (+{} more)", files.len() - named.len()));
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
        if failures > 0 {
            out.push_str(&format!(" ({failures} reported failures)"));
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
}
