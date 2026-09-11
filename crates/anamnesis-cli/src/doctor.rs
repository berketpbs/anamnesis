//! What `anamnesis doctor` answers: why does this memory look thinner than the
//! work that went into it.
//!
//! `status` answers "is my work being recorded right now" and answers it well.
//! This is the other question, and it has a different shape: the recording can
//! be working perfectly and the pages still be worth little, because of things
//! no single probe can see. A harness wired for four of five moments captures
//! sessions with holes in them. A harness that reports no tool outcome makes
//! every page count successes only. A binary older than the one being run here
//! records what an older build knew how to record. None of those look like
//! failures — they look like a quiet week.
//!
//! The judgements are a pure function over gathered facts, so each one can be
//! tested against a situation rather than against a machine.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::observation::{EventKind, RESULT_MARKER};
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::{EmbedFailure, Store, SummarySource};

use crate::hooks;

/// How many recent sessions are read to judge what capture is producing.
///
/// Enough to see past one quiet afternoon, few enough that the command stays
/// instant on a project with thousands.
const SESSIONS_EXAMINED: usize = 20;

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Working as intended. Printed because a diagnosis that only ever prints
    /// problems cannot be told from one that failed to run.
    Fine,
    /// Working, but producing less than it could.
    Thin,
    /// Something is not being recorded at all.
    Broken,
}

impl Severity {
    /// The marker this severity prints with.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Fine => "ok",
            Self::Thin => "thin",
            Self::Broken => "broken",
        }
    }
}

/// One judgement, with what it was based on and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// How bad it is.
    pub severity: Severity,
    /// What is being judged.
    pub subject: &'static str,
    /// The judgement, in one line.
    pub verdict: String,
    /// What to do, when there is something to do.
    pub remedy: Option<String>,
}

/// What was found on this machine, before any of it is judged.
///
/// Gathered by the command and passed to [`diagnose`] as data, so that a
/// situation nobody can reproduce on demand — a harness that reports no
/// outcomes, a wiki of counted pages — is still a test.
#[derive(Debug, Clone, Default)]
pub struct Symptoms {
    /// Lifecycle moments wired for each harness this project has settings for,
    /// as the parser classifies the names actually registered.
    pub wired: BTreeMap<String, Vec<EventKind>>,
    /// Harnesses whose settings file exists but wires nothing to anamnesis.
    pub unwired: Vec<String>,
    /// Sessions examined, newest first: how many observations each holds.
    pub sessions: Vec<SessionFacts>,
    /// Whether a model is configured to write pages.
    pub model: Option<String>,
    /// Which build the server answering is, when one answered.
    ///
    /// `None` means nothing answered, which `status` is the command for. This
    /// one is about the case where something did answer and is not the build
    /// the person thinks it is.
    pub server_build: Option<String>,
    /// Whether anything answered at the server's address at all.
    ///
    /// Separate from the build it named, because the interesting case is the
    /// one where those disagree: something is running and cannot say what it
    /// is, which is itself an answer.
    pub server_answered: bool,
    /// Which build is asking.
    pub this_build: String,
    /// Pages that were meant to have a vector and do not.
    ///
    /// Its own field rather than a count, because the remedy differs entirely
    /// between a model that would not load and a page that would not fit, and
    /// the reason is only in the rows.
    pub embed_failures: Vec<EmbedFailure>,
}

/// What one recent session shows about what capture is producing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFacts {
    /// Events recorded, by kind.
    pub kinds: HashMap<EventKind, usize>,
    /// Tool completions whose body carries what the tool returned.
    pub with_results: usize,
    /// Tool completions whose outcome the harness stated either way.
    pub with_outcome: usize,
    /// What wrote this session's page, once something has.
    pub summary: Option<SummarySource>,
}

impl SessionFacts {
    /// Tool completions recorded in this session.
    pub fn completions(&self) -> usize {
        self.kinds.get(&EventKind::ToolUse).copied().unwrap_or(0)
    }

    /// Whether this session carried any content at all.
    pub fn substantive(&self) -> bool {
        self.kinds
            .iter()
            .any(|(kind, count)| !kind.is_boundary_only() && *count > 0)
    }
}

/// Judge what was found.
///
/// Ordered worst first: somebody running this has a question, and the answer
/// is usually the first line.
pub fn diagnose(symptoms: &Symptoms) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(judge_hooks(symptoms));
    findings.extend(judge_capture(symptoms));
    findings.extend(judge_pages(symptoms));
    findings.extend(judge_embeddings(symptoms));
    findings.extend(judge_build(symptoms));
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
}

/// Whether every page that should carry a vector does.
///
/// Silent by design when there is nothing wrong. An embedder is opt-in, and a
/// project that never switched one on has no vectors, no failures, and no
/// business being told about either — printing "0 pages failed to embed" to
/// somebody who is not embedding is noise that trains people to skim.
fn judge_embeddings(symptoms: &Symptoms) -> Vec<Finding> {
    if symptoms.embed_failures.is_empty() {
        return Vec::new();
    }

    // One reason or several changes what to do next, so the verdict says which
    // rather than leaving it to be guessed from a count. A single recurring
    // error is a broken embedder; a spread of them is more likely the pages.
    let mut reasons: Vec<&str> = symptoms
        .embed_failures
        .iter()
        .map(|failure| failure.reason.as_str())
        .collect();
    reasons.sort_unstable();
    reasons.dedup();

    let count = symptoms.embed_failures.len();
    let pages = if count == 1 { "page" } else { "pages" };
    let first = &symptoms.embed_failures[0];

    vec![Finding {
        severity: Severity::Broken,
        subject: "embeddings",
        verdict: format!(
            "{count} {pages} are indexed without a vector and are missing from the \
             vector stream ({}, under {})",
            first.path, first.model
        ),
        remedy: Some(match reasons.as_slice() {
            [only] => format!(
                "every one failed the same way: {only} — fix that, then `anamnesis reindex` \
                 re-embeds them"
            ),
            many => format!(
                "{} different errors, the first being: {} — `anamnesis reindex` retries them all",
                many.len(),
                first.reason
            ),
        }),
    }]
}

/// Whether the harnesses are wired for every moment anamnesis records.
fn judge_hooks(symptoms: &Symptoms) -> Vec<Finding> {
    let mut findings = Vec::new();

    for agent in &symptoms.unwired {
        findings.push(Finding {
            severity: Severity::Broken,
            subject: "hooks",
            verdict: format!("{agent} is configured here but nothing is wired to anamnesis"),
            remedy: Some(format!("anamnesis install-hooks --agent {agent}")),
        });
    }

    for (agent, wired) in &symptoms.wired {
        // The pre-tool moment is what makes a failed call visible on a harness
        // that reports no outcome, which is every harness measured so far. A
        // setup wired before that existed looks entirely healthy and quietly
        // counts successes only.
        if !wired.contains(&EventKind::ToolAttempt) {
            findings.push(Finding {
                severity: Severity::Thin,
                subject: "hooks",
                verdict: format!(
                    "{agent} does not report tool calls before they run, so a call that \
                     failed leaves no trace"
                ),
                remedy: Some(format!("anamnesis install-hooks --agent {agent}")),
            });
        }

        // Not in the required list below, because a harness that does not
        // send an assistant message cannot be faulted for not being wired for
        // one. Where the moment exists and is unwired, the pages lose the only
        // part of a session written in words.
        if !wired.contains(&EventKind::AssistantMessage) {
            findings.push(Finding {
                severity: Severity::Thin,
                subject: "hooks",
                verdict: format!(
                    "{agent} does not report what the agent said when it finished, so pages are compiled from tool calls alone"
                ),
                remedy: Some(format!("anamnesis install-hooks --agent {agent}")),
            });
        }

        let missing: Vec<&str> = [
            (EventKind::SessionStart, "session starts"),
            (EventKind::UserPrompt, "prompts"),
            (EventKind::ToolUse, "tool calls"),
            (EventKind::SessionEnd, "session ends"),
        ]
        .into_iter()
        .filter(|(kind, _)| !wired.contains(kind))
        .map(|(_, name)| name)
        .collect();

        if missing.is_empty() {
            // Only when the pre-tool moment is there too, or the same harness
            // would be reported as complete on the line after it was reported
            // as unable to see a failure.
            if wired.contains(&EventKind::ToolAttempt)
                && wired.contains(&EventKind::AssistantMessage)
            {
                findings.push(Finding {
                    severity: Severity::Fine,
                    subject: "hooks",
                    verdict: format!("{agent} reports every moment anamnesis records"),
                    remedy: None,
                });
            }
        } else {
            findings.push(Finding {
                severity: Severity::Broken,
                subject: "hooks",
                verdict: format!("{agent} does not report {}", missing.join(", ")),
                remedy: Some(format!("anamnesis install-hooks --agent {agent}")),
            });
        }
    }

    if symptoms.wired.is_empty() && symptoms.unwired.is_empty() {
        findings.push(Finding {
            severity: Severity::Broken,
            subject: "hooks",
            verdict: "no harness in this project is wired to anamnesis".to_owned(),
            remedy: Some("anamnesis install-hooks".to_owned()),
        });
    }

    findings
}

/// What the recorded sessions show about the quality of what is being captured.
fn judge_capture(symptoms: &Symptoms) -> Vec<Finding> {
    let mut findings = Vec::new();

    let working: Vec<&SessionFacts> = symptoms
        .sessions
        .iter()
        .filter(|s| s.substantive())
        .collect();

    if working.is_empty() {
        findings.push(Finding {
            severity: Severity::Broken,
            subject: "capture",
            verdict: format!(
                "none of the last {} sessions recorded anything but its own start and end",
                symptoms.sessions.len()
            ),
            remedy: Some(
                "check that the server is running and that hooks point at it: anamnesis status"
                    .to_owned(),
            ),
        });
        return findings;
    }

    let completions: usize = working.iter().map(|s| s.completions()).sum();
    let with_results: usize = working.iter().map(|s| s.with_results).sum();
    let with_outcome: usize = working.iter().map(|s| s.with_outcome).sum();

    if completions > 0 && with_results == 0 {
        findings.push(Finding {
            severity: Severity::Thin,
            subject: "capture",
            verdict: format!(
                "{completions} tool calls recorded and not one of them says what it returned, \
                 so a page can only list what was attempted"
            ),
            remedy: Some(
                "the hook binary predates result capture; install the current build and \
                 restart the server"
                    .to_owned(),
            ),
        });
    } else if completions > 0 {
        findings.push(Finding {
            severity: Severity::Fine,
            subject: "capture",
            verdict: format!("{with_results} of {completions} tool calls carry what they returned"),
            remedy: None,
        });
    }

    // Not a fault, and not fixable here — it is a property of the harness. It
    // is reported because a reader of these pages otherwise reads the absence
    // of failures as evidence there were none.
    if completions > 0 && with_outcome == 0 {
        findings.push(Finding {
            severity: Severity::Thin,
            subject: "outcomes",
            verdict: "this harness never states whether a tool call succeeded; failures are \
                      inferred from calls that never came back"
                .to_owned(),
            remedy: None,
        });
    }

    findings
}

/// Whether the server recording sessions is the build that is being asked.
///
/// A version cannot answer this: `1.0.0` is the same string all release cycle,
/// so a server started weeks ago and a binary compiled a minute ago compare
/// equal. The commit stamp is what makes the difference visible, and on this
/// project the difference was the whole problem — the running server predated
/// the code that records what a tool returned, everything looked healthy, and
/// the only symptom was pages worth less than the work behind them.
fn judge_build(symptoms: &Symptoms) -> Vec<Finding> {
    let Some(server) = &symptoms.server_build else {
        // Nothing answering is `status`'s question. Something answering that
        // cannot name its build is this one's, and it is not ambiguous: the
        // endpoint has been there since builds were stamped, so a server
        // without it is older than any build that could ask.
        if symptoms.server_answered {
            return vec![Finding {
                severity: Severity::Thin,
                subject: "build",
                verdict: format!(
                    "the server is answering but cannot say which build it is, which means it predates the build stamp and is older than this one ({})",
                    symptoms.this_build
                ),
                remedy: Some(
                    "install this build where the hooks and the server run, then restart the server"
                        .to_owned(),
                ),
            }];
        }
        return Vec::new();
    };

    if server == &symptoms.this_build {
        return vec![Finding {
            severity: Severity::Fine,
            subject: "build",
            verdict: format!("the server is running this build ({server})"),
            remedy: None,
        }];
    }

    vec![Finding {
        severity: Severity::Thin,
        subject: "build",
        verdict: format!(
            "the server is running {server} and this is {} — it records what its own build knows how to record",
            symptoms.this_build
        ),
        remedy: Some(
            "install this build where the hooks and the server run, then restart the server"
                .to_owned(),
        ),
    }]
}

/// What became of the sessions that were recorded.
///
/// Judged from what actually wrote the pages rather than from this shell's
/// environment, and the difference is a gotcha this project has already been
/// caught by: the model lives in the *server's* environment. A provider
/// exported in the terminal says nothing about the process that consolidates,
/// and a terminal without one says nothing either. The pages know.
fn judge_pages(symptoms: &Symptoms) -> Vec<Finding> {
    let mut findings = Vec::new();

    let summarised: Vec<SummarySource> = symptoms
        .sessions
        .iter()
        .filter(|s| s.substantive())
        .filter_map(|s| s.summary)
        .collect();

    if summarised.is_empty() {
        return findings;
    }

    let counted = summarised
        .iter()
        .filter(|source| **source == SummarySource::Counted)
        .count();

    if counted == 0 {
        findings.push(Finding {
            severity: Severity::Fine,
            subject: "pages",
            verdict: format!("a model wrote all {} of the recent pages", summarised.len()),
            remedy: None,
        });
        return findings;
    }

    // Two situations produce identical pages — no model configured for the
    // server, and a model that was asked and failed — so the verdict names
    // both and the remedy starts where they are told apart.
    let hint = match &symptoms.model {
        Some(provider) => format!(
            " (this terminal has ANAMNESIS_LLM_PROVIDER={provider}, which the server does not inherit)"
        ),
        None => String::new(),
    };
    findings.push(Finding {
        severity: Severity::Thin,
        subject: "pages",
        verdict: format!(
            "{counted} of {} recent pages were written by counting — a tally of what happened rather than an account of it{hint}",
            summarised.len()
        ),
        remedy: Some(
            "the model lives in the server's environment, not this one: check the server log for why it fell back, then rewrite the pages with `anamnesis reconsolidate --apply`"
                .to_owned(),
        ),
    });

    findings
}

/// Gather what is true on this machine, then say what it means.
pub fn cmd_doctor(server: &str, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    let mut symptoms = Symptoms {
        model: std::env::var("ANAMNESIS_LLM_PROVIDER")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        server_answered: server_answers(server),
        server_build: server_build(server),
        this_build: anamnesis_core::build::IDENTITY.to_owned(),
        ..Symptoms::default()
    };

    for harness in hooks::HARNESSES {
        let settings: PathBuf = harness
            .settings
            .iter()
            .fold(cwd.clone(), |path, part| path.join(part));
        if !settings.exists() {
            continue;
        }
        let wired = wired_moments(&settings);
        if wired.is_empty() {
            symptoms.unwired.push(harness.agent.to_owned());
        } else {
            symptoms.wired.insert(harness.agent.to_owned(), wired);
        }
    }

    let store = Store::open(data.db_file())?;
    // The same as every other command that reads the index: a diagnosis run
    // against a database an older build wrote should describe that setup, not
    // fail on a column it has not got yet.
    store.migrate()?;
    for summary in store.recent_sessions(scope.project_id, SESSIONS_EXAMINED)? {
        let observations = store.observations(summary.id)?;
        let mut facts = SessionFacts {
            summary: summary.summary_source,
            ..SessionFacts::default()
        };
        for observation in &observations {
            *facts.kinds.entry(observation.kind).or_insert(0) += 1;
            if observation.kind != EventKind::ToolUse {
                continue;
            }
            if observation.body.as_str().contains(RESULT_MARKER) {
                facts.with_results += 1;
            }
            if observation.tool.as_ref().is_some_and(|t| t.ok.is_some()) {
                facts.with_outcome += 1;
            }
        }
        symptoms.sessions.push(facts);
    }
    symptoms.embed_failures = store.embed_failures(scope.project_id)?;

    println!("🩺 Anamnesis Memory Diagnosis");
    println!();
    println!("  Project: {}", scope.scope.project);
    println!("  Sessions examined: {}", symptoms.sessions.len());
    println!();

    for finding in diagnose(&symptoms) {
        println!(
            "  [{}] {}: {}",
            finding.severity.marker(),
            finding.subject,
            finding.verdict
        );
        if let Some(remedy) = finding.remedy {
            println!("         → {remedy}");
        }
    }

    Ok(())
}

/// Whether anything is listening at the server's address.
fn server_answers(server: &str) -> bool {
    let Ok(client) = probe_client() else {
        return false;
    };
    client
        .get(format!("{server}/health"))
        .send()
        .is_ok_and(|response| response.status().is_success())
}

/// The client both probes use: quick to give up, because a person is watching.
fn probe_client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
}

/// Ask the server which build it is.
///
/// Nothing answering is not this command's problem — `status` exists to tell a
/// stopped server from a refused one — so every failure here is the same
/// `None`, and the finding it produces is no finding at all.
fn server_build(server: &str) -> Option<String> {
    let client = probe_client().ok()?;
    let response = client.get(format!("{server}/version")).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().ok()?;
    body.get("identity")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The lifecycle moments a settings file wires to anamnesis.
///
/// Classified through the parser rather than by matching names here: a hook
/// registered under a name the parser does not recognise is captured as an
/// unclassified notification, and this command exists to notice exactly that
/// kind of quiet gap.
fn wired_moments(settings: &std::path::Path) -> Vec<EventKind> {
    // Through the same reader `install-hooks` uses, so a file it can write is
    // a file this can read.
    let Ok(value) = hooks::read_settings(settings) else {
        return Vec::new();
    };
    let Some(hooks) = value.get("hooks").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };

    let mut moments = Vec::new();
    for (event, matchers) in hooks {
        let mentions_anamnesis = matchers
            .to_string()
            .to_ascii_lowercase()
            .contains("anamnesis");
        if !mentions_anamnesis {
            continue;
        }
        let kind = anamnesis_hooks::classify_event(event);
        if !moments.contains(&kind) {
            moments.push(kind);
        }
    }
    moments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(kinds: &[(EventKind, usize)]) -> SessionFacts {
        SessionFacts {
            kinds: kinds.iter().copied().collect(),
            ..SessionFacts::default()
        }
    }

    fn embed_failure(path: &str, reason: &str) -> EmbedFailure {
        EmbedFailure {
            path: anamnesis_core::page::PagePath::parse(path).expect("path"),
            title: "A page".to_owned(),
            model: "all-MiniLM-L6-v2".to_owned(),
            at: "2026-09-11T12:00:00Z".to_owned(),
            reason: reason.to_owned(),
        }
    }

    fn wired(agent: &str, moments: &[EventKind]) -> Symptoms {
        let mut symptoms = Symptoms {
            model: Some("gemini-2.5-flash".to_owned()),
            ..Symptoms::default()
        };
        symptoms.wired.insert(agent.to_owned(), moments.to_vec());
        symptoms
    }

    const EVERY_MOMENT: [EventKind; 6] = [
        EventKind::SessionStart,
        EventKind::UserPrompt,
        EventKind::ToolAttempt,
        EventKind::ToolUse,
        EventKind::AssistantMessage,
        EventKind::SessionEnd,
    ];

    /// A string continued across source lines keeps the indentation of the
    /// next line unless the continuation is written exactly right, and a
    /// verdict reading `pages are            compiled` has happened three
    /// times in this file's history. It is invisible in review and obvious to
    /// the person the line is printed to.
    #[test]
    fn no_verdict_carries_the_indentation_of_the_source_it_was_written_in() {
        let mut symptoms = wired("claude-code", &[EventKind::SessionStart]);
        symptoms.unwired.push("codex".to_owned());
        symptoms.server_answered = true;
        symptoms.sessions = vec![SessionFacts {
            summary: Some(SummarySource::Counted),
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 9)])
        }];

        for finding in diagnose(&symptoms) {
            assert!(
                !finding.verdict.contains("  "),
                "{}: {:?}",
                finding.subject,
                finding.verdict
            );
            if let Some(remedy) = &finding.remedy {
                assert!(!remedy.contains("  "), "{remedy:?}");
            }
        }
    }

    /// A setup wired before the assistant's own account was captured looks
    /// complete and produces pages compiled from tool calls alone.
    #[test]
    fn a_setup_that_never_hears_the_agent_speak_is_reported_as_thin() {
        let symptoms = wired(
            "claude-code",
            &[
                EventKind::SessionStart,
                EventKind::UserPrompt,
                EventKind::ToolAttempt,
                EventKind::ToolUse,
                EventKind::SessionEnd,
            ],
        );

        let findings = diagnose(&symptoms);

        assert!(
            findings.iter().any(|f| f.subject == "hooks"
                && f.verdict.contains("what the agent said when it finished")),
            "{findings:#?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.verdict.contains("reports every moment")),
            "{findings:#?}"
        );
    }

    /// The gap this command was written for: everything looks healthy, and
    /// failures have been invisible for as long as the setup has existed.
    #[test]
    fn a_setup_that_cannot_see_a_failed_call_is_reported_as_thin() {
        let mut symptoms = wired(
            "claude-code",
            &[
                EventKind::SessionStart,
                EventKind::UserPrompt,
                EventKind::ToolUse,
                EventKind::SessionEnd,
            ],
        );
        symptoms.sessions = vec![SessionFacts {
            with_results: 4,
            with_outcome: 0,
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 4)])
        }];

        let findings = diagnose(&symptoms);

        assert!(
            findings.iter().any(|f| f.subject == "hooks"
                && f.severity == Severity::Thin
                && f.verdict.contains("a call that failed leaves no trace")),
            "{findings:#?}"
        );
    }

    /// A binary older than result capture records what it knew how to record,
    /// and nothing about the setup says so.
    #[test]
    fn tool_calls_with_no_results_name_the_binary_as_the_cause() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sessions = vec![SessionFacts {
            with_results: 0,
            with_outcome: 0,
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 12)])
        }];

        let findings = diagnose(&symptoms);
        let capture = findings
            .iter()
            .find(|f| f.subject == "capture")
            .expect("a capture finding");

        assert_eq!(capture.severity, Severity::Thin);
        assert!(
            capture
                .verdict
                .contains("not one of them says what it returned")
        );
        assert!(
            capture
                .remedy
                .as_ref()
                .unwrap()
                .contains("install the current build")
        );
    }

    /// Sessions that hold only their own boundaries are what a broken hook
    /// chain looks like from the index: rows arriving, nothing in them.
    #[test]
    fn sessions_holding_only_boundaries_are_reported_as_broken() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sessions = vec![
            session(&[(EventKind::SessionStart, 1), (EventKind::SessionEnd, 1)]),
            session(&[(EventKind::SessionStart, 1), (EventKind::SessionEnd, 1)]),
        ];

        let findings = diagnose(&symptoms);

        assert_eq!(findings.first().map(|f| f.severity), Some(Severity::Broken));
        assert!(
            findings[0]
                .verdict
                .contains("anything but its own start and end")
        );
    }

    /// An embedder is opt-in. A project that never switched one on has no
    /// vectors, no failures, and no business being told about either.
    #[test]
    fn a_project_with_no_embedding_failures_hears_nothing_about_embeddings() {
        let symptoms = wired("claude-code", &EVERY_MOMENT);

        assert!(
            diagnose(&symptoms)
                .iter()
                .all(|f| f.subject != "embeddings")
        );
    }

    /// The page is in the wiki, in the index, and in three of four streams.
    /// Nothing else in `doctor` would have said a word about it.
    #[test]
    fn pages_missing_a_vector_are_reported_as_broken_and_named() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.embed_failures = vec![embed_failure("decisions/0001-storage.md", "no model")];

        let findings = diagnose(&symptoms);

        let embeddings = findings
            .iter()
            .find(|f| f.subject == "embeddings")
            .expect("an embeddings finding");
        assert_eq!(embeddings.severity, Severity::Broken);
        assert!(
            embeddings.verdict.contains("decisions/0001-storage.md"),
            "a report that cannot name the page is one nobody can act on: {embeddings:#?}"
        );
        assert!(
            embeddings.verdict.contains("all-MiniLM-L6-v2"),
            "{embeddings:#?}"
        );
        assert!(embeddings.verdict.contains("1 page"), "{embeddings:#?}");
    }

    /// One recurring error is a broken embedder; a spread of them is more
    /// likely the pages. The remedy differs, so the verdict has to tell them
    /// apart rather than leave it to be guessed from a count.
    #[test]
    fn one_shared_reason_reads_differently_from_several() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);

        symptoms.embed_failures = vec![
            embed_failure("notes/a.md", "no model loaded"),
            embed_failure("notes/b.md", "no model loaded"),
        ];
        let same = diagnose(&symptoms)
            .into_iter()
            .find(|f| f.subject == "embeddings")
            .expect("finding");
        assert!(same.verdict.contains("2 pages"), "{same:#?}");
        let remedy = same.remedy.as_deref().expect("a remedy");
        assert!(remedy.contains("every one failed the same way"), "{remedy}");
        assert!(remedy.contains("no model loaded"), "{remedy}");

        symptoms.embed_failures = vec![
            embed_failure("notes/a.md", "no model loaded"),
            embed_failure("notes/b.md", "input too long"),
        ];
        let mixed = diagnose(&symptoms)
            .into_iter()
            .find(|f| f.subject == "embeddings")
            .expect("finding");
        let remedy = mixed.remedy.as_deref().expect("a remedy");
        assert!(remedy.contains("2 different errors"), "{remedy}");
    }

    /// A model that was asked and could not answer leaves counted pages, and
    /// counted pages are indistinguishable from a project with no model. The
    /// verdict is drawn from what wrote the pages, never from this shell's
    /// environment — the model lives in the server's.
    #[test]
    fn pages_written_by_counting_are_reported_from_the_pages_themselves() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sessions = vec![SessionFacts {
            summary: Some(SummarySource::Counted),
            with_results: 3,
            with_outcome: 0,
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 3)])
        }];

        let findings = diagnose(&symptoms);

        let pages = findings
            .iter()
            .find(|f| f.subject == "pages")
            .expect("a pages finding");
        assert_eq!(pages.severity, Severity::Thin);
        assert!(pages.verdict.contains("written by counting"), "{pages:#?}");
        assert!(
            pages
                .remedy
                .as_ref()
                .unwrap()
                .contains("server's environment"),
            "{pages:#?}"
        );
    }

    /// A terminal with no provider exported says nothing about the server, so
    /// a project whose pages a model wrote is reported as healthy even though
    /// nothing here can see a model at all.
    #[test]
    fn a_shell_without_a_provider_is_not_evidence_against_the_server() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.model = None;
        symptoms.sessions = vec![SessionFacts {
            summary: Some(SummarySource::Model),
            with_results: 3,
            with_outcome: 0,
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 3)])
        }];

        let findings = diagnose(&symptoms);

        assert!(
            findings.iter().all(|f| f.severity != Severity::Broken),
            "{findings:#?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.subject == "pages" && f.verdict.contains("a model wrote all")),
            "{findings:#?}"
        );
    }

    /// One harness cannot be complete and blind at once. The line that says a
    /// setup reports every moment is held back until it does.
    #[test]
    fn a_harness_missing_the_pre_tool_moment_is_not_also_called_complete() {
        let symptoms = wired(
            "claude-code",
            &[
                EventKind::SessionStart,
                EventKind::UserPrompt,
                EventKind::ToolUse,
                EventKind::SessionEnd,
            ],
        );

        let findings = diagnose(&symptoms);

        assert!(
            !findings
                .iter()
                .any(|f| f.verdict.contains("reports every moment")),
            "{findings:#?}"
        );
    }

    /// A healthy setup still prints. A diagnosis that says nothing when
    /// nothing is wrong cannot be told from one that failed to run.
    #[test]
    fn a_healthy_setup_is_still_reported() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sessions = vec![SessionFacts {
            summary: Some(SummarySource::Model),
            with_results: 6,
            with_outcome: 6,
            ..session(&[(EventKind::UserPrompt, 2), (EventKind::ToolUse, 6)])
        }];

        let findings = diagnose(&symptoms);

        assert!(!findings.is_empty());
        assert!(
            findings.iter().all(|f| f.severity == Severity::Fine),
            "{findings:#?}"
        );
    }

    /// The gap that cost this project weeks of page quality: a server running
    /// a build older than the code, with every other signal healthy.
    #[test]
    fn a_server_running_another_build_is_named_as_one() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.this_build = "1.0.0 (2baf7c7)".to_owned();
        symptoms.server_answered = true;
        symptoms.server_build = Some("1.0.0 (83745dc)".to_owned());
        symptoms.sessions = vec![SessionFacts {
            summary: Some(SummarySource::Model),
            with_results: 2,
            with_outcome: 0,
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 2)])
        }];

        let findings = diagnose(&symptoms);
        let build = findings
            .iter()
            .find(|f| f.subject == "build")
            .expect("a build finding");

        assert_eq!(build.severity, Severity::Thin);
        assert!(build.verdict.contains("83745dc"), "{build:#?}");
        assert!(build.verdict.contains("2baf7c7"), "{build:#?}");
    }

    /// The same version on both sides is not evidence of the same build, which
    /// is the entire reason the commit is stamped: `1.0.0` never moves.
    #[test]
    fn a_matching_version_with_a_different_commit_is_still_a_mismatch() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.this_build = "1.0.0 (aaaaaaa)".to_owned();
        symptoms.server_answered = true;
        symptoms.server_build = Some("1.0.0 (bbbbbbb)".to_owned());

        assert!(
            diagnose(&symptoms)
                .iter()
                .any(|f| f.subject == "build" && f.severity == Severity::Thin)
        );
    }

    /// Nothing answering is `status`'s question, not this one. A server that
    /// is simply not running must not be reported here as a stale build.
    #[test]
    fn a_server_that_did_not_answer_produces_no_build_finding() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.server_build = None;
        symptoms.server_answered = false;

        assert!(diagnose(&symptoms).iter().all(|f| f.subject != "build"));
    }

    /// The case this project was actually in: a server old enough that it does
    /// not know how to say which build it is. Silence from that endpoint is
    /// not missing information — it is the answer.
    #[test]
    fn a_server_that_cannot_name_its_build_is_older_than_this_one() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.server_answered = true;
        symptoms.server_build = None;

        let build = diagnose(&symptoms)
            .into_iter()
            .find(|f| f.subject == "build")
            .expect("a build finding");

        assert_eq!(build.severity, Severity::Thin);
        assert!(
            build.verdict.contains("predates the build stamp"),
            "{build:#?}"
        );
    }

    /// Nothing wired at all is the first thing a new project gets wrong, and
    /// it is the one case where every other check would report a quiet week.
    #[test]
    fn a_project_with_no_hooks_is_told_so_first() {
        let symptoms = Symptoms {
            model: Some("gemini-2.5-flash".to_owned()),
            ..Symptoms::default()
        };

        let findings = diagnose(&symptoms);

        assert_eq!(findings[0].severity, Severity::Broken);
        assert!(findings[0].verdict.contains("no harness"));
    }
}
