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
use anamnesis_store::{Store, SummarySource};

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
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
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
            if wired.contains(&EventKind::ToolAttempt) {
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
            " (this terminal has ANAMNESIS_LLM_PROVIDER={provider}, which the server does not              inherit)"
        ),
        None => String::new(),
    };
    findings.push(Finding {
        severity: Severity::Thin,
        subject: "pages",
        verdict: format!(
            "{counted} of {} recent pages were written by counting — a tally of what happened              rather than an account of it{hint}",
            summarised.len()
        ),
        remedy: Some(
            "the model lives in the server's environment, not this one: check the server log              for why it fell back, then rewrite the pages with `anamnesis reconsolidate --apply`"
                .to_owned(),
        ),
    });

    findings
}

/// Gather what is true on this machine, then say what it means.
pub fn cmd_doctor(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    let mut symptoms = Symptoms {
        model: std::env::var("ANAMNESIS_LLM_PROVIDER")
            .ok()
            .filter(|value| !value.trim().is_empty()),
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

    fn wired(agent: &str, moments: &[EventKind]) -> Symptoms {
        let mut symptoms = Symptoms {
            model: Some("gemini-2.5-flash".to_owned()),
            ..Symptoms::default()
        };
        symptoms.wired.insert(agent.to_owned(), moments.to_vec());
        symptoms
    }

    const EVERY_MOMENT: [EventKind; 5] = [
        EventKind::SessionStart,
        EventKind::UserPrompt,
        EventKind::ToolAttempt,
        EventKind::ToolUse,
        EventKind::SessionEnd,
    ];

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
