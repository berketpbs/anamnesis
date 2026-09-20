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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::embedding::MAX_SECTIONS;
use anamnesis_core::observation::{EventKind, RESULT_MARKER};
use anamnesis_core::retrieval::Tuning;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::{EmbedFailure, EmbedFault, Store, SummarySource};

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
    /// A secret is stored where the redaction rules say it must not be.
    ///
    /// Above `Broken` because the harm is already done rather than pending: a
    /// memory that records nothing loses the future, a memory holding a key
    /// has handed out the past to anyone who reads the directory or a backup.
    Exposed,
}

impl Severity {
    /// The marker this severity prints with.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Fine => "ok",
            Self::Thin => "thin",
            Self::Broken => "broken",
            Self::Exposed => "exposed",
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
    /// Harnesses that have actually recorded something in this project.
    ///
    /// [`Symptoms::wired`] is read from settings files and says what *should*
    /// arrive; this is read from the index and says what did. They come apart
    /// in practice, and the gap is invisible to every other check here: a
    /// sandboxed harness that cannot reach the binary its settings file names
    /// runs every hook and fails every one, leaving a file that reads as
    /// perfectly wired and an index with nothing in it. Judged from the file
    /// alone that is a healthy setup having a quiet week.
    ///
    /// Membership only, not recency. Whether a harness that did record has
    /// since gone silent is a question about ages, which `status` answers per
    /// agent; this is the coarser question of whether its hooks have ever run
    /// at all.
    pub captured: BTreeSet<String>,
    /// Sessions examined, newest first: how many observations each holds.
    pub sessions: Vec<SessionFacts>,
    /// What the server's model answered the last time it did not answer with
    /// a page, as `/whoami` reports it: the model's name and the reason,
    /// `gemini-3.5-flash answered 400: Please pass a valid API key`.
    ///
    /// `None` when the server's latest request was answered, when nothing
    /// answered, and for a server too old to say.
    pub server_model_failure: Option<String>,
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
    /// Whether retrieval compares a long page's sections, or only its opening.
    ///
    /// Read from the tuning that ships rather than assumed, because it decides
    /// whether a truncated page with sections is thin at all: compared, its
    /// sections stand for all of it.
    pub sections_compared: bool,
    /// The embedding model the server said it uses, when it said.
    ///
    /// A complaint row is about a vector under one model, and the index keeps
    /// every model's rows: a machine that moved from MiniLM to another embedder
    /// still holds MiniLM's truncations, which describe vectors no query
    /// compares any more. Judging those would report a fault the running
    /// system does not have. `None` — nothing answered, or a server too old to
    /// say — judges every row, as before, rather than guessing which is live.
    pub server_embedding: Option<String>,
    /// What today's redaction rules would still mask in stored observations,
    /// across every project in the index.
    ///
    /// Capture redacts once, with the rules of the day; a rule added later
    /// never reaches what came before it. This is the count of what it did not
    /// reach.
    pub stored_secrets: anamnesis_store::Redaction,
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
    findings.extend(judge_stored_secrets(symptoms));
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
}

/// Whether anything stored holds a secret today's rules would mask.
///
/// Silent when nothing does: redaction working is the ordinary case, and a line
/// saying so on every run is a line people learn to skip.
fn judge_stored_secrets(symptoms: &Symptoms) -> Vec<Finding> {
    let found = &symptoms.stored_secrets;
    if found.changed == 0 {
        return Vec::new();
    }
    let rules = found
        .rules
        .iter()
        .map(|(rule, count)| format!("{rule} ×{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    vec![Finding {
        severity: Severity::Exposed,
        subject: "secrets",
        verdict: format!(
            "{} stored observation(s) hold something today's redaction rules mask ({rules}) — \
             captured before the rule existed",
            found.changed
        ),
        remedy: Some(
            "`anamnesis redact` to see where, `anamnesis redact --apply` to mask them; then \
             revoke the credential, since it has been on disk, and replace older backups"
                .to_owned(),
        ),
    }]
}

/// Whether every page that should carry a vector carries a whole one.
///
/// Silent by design when there is nothing wrong. An embedder is opt-in, and a
/// project that never switched one on has no vectors, no complaints, and no
/// business being told about either — printing "0 pages failed to embed" to
/// somebody who is not embedding is noise that trains people to skim.
///
/// Two faults, reported separately, because they are not the same news.
///
/// A page with no vector is absent from a stream — `Broken`, by the enum's own
/// reading of "something is not being recorded at all". A page embedded from
/// only its opening tokens — 128 of them, for the default model — is present in
/// every stream and answering with part of itself, which is `Thin`: working,
/// producing less than it could.
/// Folding them together would rank the quieter one as an emergency and, worse,
/// let one remedy stand in for two that share nothing.
fn judge_embeddings(symptoms: &Symptoms) -> Vec<Finding> {
    let (truncated, failed): (Vec<&EmbedFailure>, Vec<&EmbedFailure>) = symptoms
        .embed_failures
        .iter()
        .filter(|failure| {
            symptoms
                .server_embedding
                .as_ref()
                .is_none_or(|model| failure.model == *model)
        })
        .partition(|failure| failure.kind == EmbedFault::Truncated);

    let mut findings = Vec::new();
    if !failed.is_empty() {
        findings.push(judge_failed(&failed));
    }
    findings.extend(judge_truncated(&truncated, symptoms.sections_compared));
    findings
}

/// Pages the embedder refused outright.
fn judge_failed(failed: &[&EmbedFailure]) -> Finding {
    // One reason or several changes what to do next, so the verdict says which
    // rather than leaving it to be guessed from a count. A single recurring
    // error is a broken embedder; a spread of them is more likely the pages.
    let mut reasons: Vec<&str> = failed
        .iter()
        .map(|failure| failure.reason.as_str())
        .collect();
    reasons.sort_unstable();
    reasons.dedup();

    let count = failed.len();
    let pages = if count == 1 { "page" } else { "pages" };
    let first = failed[0];

    Finding {
        severity: Severity::Broken,
        subject: "embeddings",
        verdict: format!(
            "{count} {pages} are indexed without a vector and are missing from the \
             vector stream ({}, under {})",
            first.path, first.model
        ),
        // A running server sends these pages again within a minute of its
        // embedder answering, so the first thing to fix is the embedder; the
        // rebuild is for a memory no server is running for.
        remedy: Some(match reasons.as_slice() {
            [only] => format!(
                "every one failed the same way: {only} — fix that, and a running server \
                 re-embeds them within a minute (`anamnesis reindex` does it without one)"
            ),
            many => format!(
                "{} different errors, the first being: {} — a running server retries them \
                 once its embedder answers, and `anamnesis reindex` retries them all now",
                many.len(),
                first.reason
            ),
        }),
    }
}

/// Pages longer than the model that embedded them.
///
/// The verdict leads with the *worst* page rather than the first, because the
/// question somebody has is how bad this gets, and a list ordered by when it
/// happened answers a different one.
///
/// A page that is also embedded in sections is only whole to a retrieval that
/// compares them. Where one does, it is not thin and is not counted; where
/// none does, it is as thin as a page without them, and the remedy says which
/// pages have sections — so that the one thing `anamnesis reindex` can add is
/// not recommended for pages that already hold it, nor left unsaid for pages
/// that do not.
fn judge_truncated(truncated: &[&EmbedFailure], sections_compared: bool) -> Option<Finding> {
    let thin: Vec<&EmbedFailure> = truncated
        .iter()
        .copied()
        .filter(|failure| !(sections_compared && failure.sections > 0))
        .collect();
    let count = thin.len();
    let pages = if count == 1 { "page" } else { "pages" };

    let worst = thin
        .iter()
        .max_by_key(|failure| failure.tokens.unwrap_or(0))?;
    let budget = worst.budget.unwrap_or(0);
    let detail = match (worst.tokens, worst.budget) {
        (Some(tokens), Some(budget)) if tokens > budget => format!(
            "worst is {} at {tokens} tokens against {budget}, so {} of it is outside its own vector",
            worst.path,
            tokens - budget
        ),
        _ => format!("worst is {}", worst.path),
    };

    let mut remedy = format!(
        "nothing is lost from full-text, entity or link retrieval — only the vector \
         stream sees part of these pages. A model with a longer window, or shorter \
         pages, is the fix; `{budget}`-token inputs are what the current one reads"
    );
    // Only possible when sections are not compared: otherwise these pages
    // were filtered out above.
    let sectioned = thin.iter().filter(|failure| failure.sections > 0).count();
    if sectioned > 0 {
        let (who, verb) = which_of(sectioned, count);
        remedy.push_str(&format!(
            ". {who} {verb} also embedded in sections that cover the whole page, \
             which retrieval as it ships does not compare"
        ));
    }
    let unsectioned = count - sectioned;
    if unsectioned > 0 {
        let (who, _) = which_of(unsectioned, count);
        let has = if unsectioned == 1 { "has" } else { "have" };
        remedy.push_str(&format!(
            ". {who} {has} no sections: `anamnesis reindex`, with embedding on, gives \
             them to a page embedded before sections existed, and a page that needs \
             more than {MAX_SECTIONS} keeps its opening only"
        ));
    }

    Some(Finding {
        // Thin, not Broken. These pages have vectors and are whole in full
        // text, entities and links; what they are is a fourth stream answering
        // about part of them. Calling that broken would spend the word.
        severity: Severity::Thin,
        subject: "embeddings",
        verdict: format!(
            "{count} {pages} are embedded from only as much of themselves as the model \
             could read ({detail})"
        ),
        remedy: Some(remedy),
    })
}

/// How to name `part` of `whole` pages, with the verb that agrees with it.
fn which_of(part: usize, whole: usize) -> (String, &'static str) {
    let verb = if part == 1 { "is" } else { "are" };
    let who = match (part, whole) {
        (1, 1) => "It".to_owned(),
        (part, whole) if part == whole => format!("All {whole}"),
        (part, _) => format!("{part} of them"),
    };
    (who, verb)
}

/// The other harnesses that have recorded here, named for a verdict.
///
/// `None` when there are none, which is the case that means the opposite
/// thing: an empty index is a project nobody has opened, not a harness that
/// is failing.
fn witnesses(agent: &str, captured: &BTreeSet<String>) -> Option<String> {
    let others: Vec<&str> = captured
        .iter()
        .map(String::as_str)
        .filter(|other| *other != agent)
        .collect();
    match others.as_slice() {
        [] => None,
        [one] => Some((*one).to_owned()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
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
        // Asked before anything about *which* moments are wired, because the
        // answer decides whether that question means anything. A harness whose
        // hooks never start has a settings file naming all eight events and an
        // index holding none of them, and every judgement below reads only the
        // file — so the worst-configured setup in this module and a perfect one
        // that cannot run produce the same line. Measured on 2026-09-21: Codex
        // wired to a binary outside its sandbox ran every hook, failed every
        // hook, recorded nothing, and was reported here as reporting every
        // moment anamnesis records.
        if !symptoms.captured.contains(agent) {
            // Nothing anywhere has recorded is a project nobody has used yet,
            // which is not evidence against this harness. One that records
            // while this one stays empty is: the events are arriving, from
            // something else, and this is the side that is silent.
            let (severity, because) = match witnesses(agent, &symptoms.captured) {
                Some(others) => (
                    Severity::Broken,
                    format!(", while {others} has recorded here"),
                ),
                None => (Severity::Thin, String::new()),
            };
            findings.push(Finding {
                severity,
                subject: "hooks",
                verdict: format!(
                    "{agent} is wired for every moment but has never recorded one{because}"
                ),
                // Not `install-hooks`: the hooks are installed, which is the
                // whole difficulty. What is unknown is why they do not run,
                // and the command that says so is the hook itself, run the way
                // the harness runs it.
                remedy: Some(format!(
                    "run the hook command in `{}` by hand from the project root to see why",
                    hooks::harness(agent)
                        .map(|h| h.settings.join("/"))
                        .unwrap_or_else(|| "its settings file".to_owned()),
                )),
            });
            // The per-event judgements below are all about the shape of what
            // arrives, and nothing arrives. Printing them here would bury the
            // one finding that matters under three that cannot be acted on.
            continue;
        }

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
                 restart the server with `anamnesis service restart`"
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
                remedy: Some(INSTALL_AND_RESTART.to_owned()),
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
        remedy: Some(INSTALL_AND_RESTART.to_owned()),
    }]
}

/// What to do about a server running another build.
const INSTALL_AND_RESTART: &str = "install this build where the hooks and the server run, then \
                                   restart the server with `anamnesis service restart`";

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

    // The server is the one process that asks the model, so when it heard a
    // refusal that is the reason, and the remedy follows from it. This used to
    // point at the terminal's environment ("which the server does not
    // inherit"), which stopped being true when every command started reading
    // `settings.env` and the credential store: on 2026-09-15 it sent somebody
    // whose key had been refused for a day to compare environments.
    let (hint, remedy) = match &symptoms.server_model_failure {
        Some(failure) if refuses_the_key(failure) => (
            format!("; the server's model: {failure}"),
            "the key was refused: store a new one with `anamnesis key set ANAMNESIS_LLM_API_KEY`, \
             confirm it with `anamnesis key check`, and restart the server with `anamnesis \
             service restart` — its next passes rewrite the counted pages"
                .to_owned(),
        ),
        Some(failure) => (
            format!("; the server's model: {failure}"),
            "once the model answers, the server rewrites the counted pages on its own passes; \
             `anamnesis key check` asks it now, and `anamnesis reconsolidate --apply` rewrites \
             them at once"
                .to_owned(),
        ),
        None => (
            String::new(),
            "`anamnesis key check` asks the configured model whether it answers, and the server \
             log says why a page fell back; then `anamnesis reconsolidate --apply` rewrites them"
                .to_owned(),
        ),
    };
    findings.push(Finding {
        severity: Severity::Thin,
        subject: "pages",
        verdict: format!(
            "{counted} of {} recent pages were written by counting — a tally of what happened rather than an account of it{hint}",
            summarised.len()
        ),
        remedy: Some(remedy),
    });

    findings
}

/// Whether a reason the server reported is a refused credential: a 401 or a
/// 403, or a 400 whose sentence names the key — Google's way of saying it.
pub(crate) fn refuses_the_key(failure: &str) -> bool {
    let lower = failure.to_ascii_lowercase();
    lower.contains("answered 401")
        || lower.contains("answered 403")
        || (lower.contains("answered 400")
            && (lower.contains("api key") || lower.contains("auth key")))
}

/// Gather what is true on this machine, then say what it means.
pub fn cmd_doctor(server: &str, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    let whoami = server_whoami(server);
    let mut symptoms = Symptoms {
        server_model_failure: whoami.as_ref().and_then(model_failure),
        server_answered: server_answers(server),
        server_build: server_build(server),
        server_embedding: whoami.as_ref().and_then(|body| {
            body.get("embedding")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }),
        this_build: anamnesis_core::build::IDENTITY.to_owned(),
        ..Symptoms::default()
    };

    // From the project root, not the working directory: a diagnosis run in a
    // subdirectory of a wired project would otherwise find no settings there,
    // call the project unwired while it is recording, and send the person to
    // `install-hooks` — which used to write the file into that subdirectory,
    // where no harness reads it, and silence the alarm with nothing behind it.
    for harness in hooks::HARNESSES {
        let settings: PathBuf = hooks::default_settings_path(&harness, &scope.root);
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
    symptoms.captured = store
        .last_capture_by_agent(scope.project_id)?
        .into_iter()
        .map(|(agent, _)| agent)
        .collect();
    symptoms.embed_failures = store.embed_failures(scope.project_id)?;
    symptoms.sections_compared = Tuning::default().vector_sections;
    symptoms.stored_secrets =
        store.redact_observations(&anamnesis_core::sanitize::Redactor::new(), false)?;

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

/// Ask the server what it does: which model it embeds with, and what its
/// consolidation model last answered when it did not write a page.
///
/// A token is sent when this shell has one, since a server that requires
/// tokens answers nothing without it. Any failure is `None`: for the embedder
/// that judges every complaint row rather than none, and for the model it is
/// no reason rather than a guessed one.
pub(crate) fn server_whoami(server: &str) -> Option<serde_json::Value> {
    let client = probe_client().ok()?;
    let mut request = client.get(format!("{server}/whoami"));
    if let Ok(token) = std::env::var(anamnesis_web::auth::TOKEN_ENV) {
        request = request.bearer_auth(token);
    }
    let response = request.send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().ok()
}

/// `gemini-3.5-flash answered 400: ...`, from a `/whoami` body.
pub(crate) fn model_failure(body: &serde_json::Value) -> Option<String> {
    let reason = body.get("consolidation_failure")?.get("reason")?.as_str()?;
    Some(
        match body
            .get("consolidation")
            .and_then(serde_json::Value::as_str)
        {
            Some(model) => format!("{model} {reason}"),
            None => reason.to_owned(),
        },
    )
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
            kind: EmbedFault::Failed,
            tokens: None,
            budget: None,
            path: anamnesis_core::page::PagePath::parse(path).expect("path"),
            title: "A page".to_owned(),
            model: "all-MiniLM-L6-v2".to_owned(),
            at: "2026-09-11T12:00:00Z".to_owned(),
            reason: reason.to_owned(),
            sections: 0,
        }
    }

    fn embed_truncation(path: &str, tokens: usize) -> EmbedFailure {
        EmbedFailure {
            kind: EmbedFault::Truncated,
            tokens: Some(tokens),
            budget: Some(512),
            ..embed_failure(path, "longer than the window")
        }
    }

    /// A harness wired for `moments` whose hooks do reach the index.
    ///
    /// Capture is part of the fixture rather than left out of it because
    /// every judgement about *which* moments are wired presumes the wiring
    /// runs at all; a fixture without it tests those judgements against a
    /// setup where they no longer apply.
    fn wired(agent: &str, moments: &[EventKind]) -> Symptoms {
        let mut symptoms = Symptoms::default();
        symptoms.wired.insert(agent.to_owned(), moments.to_vec());
        symptoms.captured.insert(agent.to_owned());
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

    /// The finding this module was missing: settings that name every moment,
    /// and an index that holds none of them. Codex on Windows, 2026-09-21 —
    /// its sandbox could not reach the binary the hook command named, so every
    /// hook ran and every hook failed, and the report called it healthy.
    #[test]
    fn a_wired_harness_that_never_recorded_is_not_called_healthy() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms
            .wired
            .insert("codex".to_owned(), EVERY_MOMENT.to_vec());

        let hooks: Vec<Finding> = diagnose(&symptoms)
            .into_iter()
            .filter(|f| f.subject == "hooks")
            .collect();

        let codex = hooks
            .iter()
            .find(|f| f.verdict.starts_with("codex"))
            .expect("a finding about codex");
        assert_eq!(codex.severity, Severity::Broken, "{codex:?}");
        assert!(codex.verdict.contains("never recorded one"), "{codex:?}");
        // The harness that does work is named, because it is the evidence:
        // the events are arriving, and codex is not the one sending them.
        assert!(codex.verdict.contains("claude-code"), "{codex:?}");
        assert!(
            !hooks
                .iter()
                .any(|f| f.verdict.starts_with("codex") && f.severity == Severity::Fine),
            "a silent harness was also reported as fine: {hooks:?}"
        );
    }

    /// The opposite case, which must not be reported the same way. A project
    /// nobody has opened has an empty index for a reason that says nothing
    /// about any harness in it, and calling that broken would make the check
    /// fire on every fresh `install-hooks`.
    #[test]
    fn a_fresh_project_with_nothing_recorded_anywhere_is_only_thin() {
        let mut symptoms = Symptoms::default();
        symptoms
            .wired
            .insert("codex".to_owned(), EVERY_MOMENT.to_vec());

        let finding = diagnose(&symptoms)
            .into_iter()
            .find(|f| f.subject == "hooks")
            .expect("a finding about hooks");
        assert_eq!(finding.severity, Severity::Thin, "{finding:?}");
        // Nothing to name, so nothing is named: no other harness is implied
        // to be working.
        assert!(!finding.verdict.contains("while"), "{finding:?}");
    }

    /// Nothing arriving is one finding, not four. The per-event judgements
    /// all describe the shape of what does arrive, and are noise when the
    /// answer is that none of it does.
    #[test]
    fn a_silent_harness_is_not_also_faulted_for_the_moments_it_lacks() {
        let mut symptoms = Symptoms::default();
        symptoms
            .wired
            .insert("codex".to_owned(), vec![EventKind::SessionStart]);
        symptoms.captured.insert("claude-code".to_owned());

        let hooks: Vec<String> = diagnose(&symptoms)
            .into_iter()
            .filter(|f| f.subject == "hooks")
            .map(|f| f.verdict)
            .collect();
        assert_eq!(hooks.len(), 1, "{hooks:?}");
        assert!(hooks[0].contains("never recorded one"), "{hooks:?}");
    }

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
        let remedy = embeddings.remedy.as_deref().expect("a remedy");
        assert!(
            remedy.contains("running server re-embeds them"),
            "the server fills these in by itself now, so fixing the embedder is the step: {remedy}"
        );
    }

    /// A page embedded from only its opening tokens is in every stream and
    /// answering with part of itself. That is `Thin` — working, producing
    /// less than it could — and calling it `Broken` would spend the word that
    /// means a page is absent.
    #[test]
    fn a_truncated_page_is_thin_rather_than_broken() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.embed_failures = vec![embed_truncation("gotchas/long.md", 1341)];

        let finding = diagnose(&symptoms)
            .into_iter()
            .find(|f| f.subject == "embeddings")
            .expect("an embeddings finding");

        assert_eq!(finding.severity, Severity::Thin);
        assert!(finding.verdict.contains("gotchas/long.md"), "{finding:#?}");
        assert!(finding.verdict.contains("1341"), "{finding:#?}");
        // 1341 − 512: the part of the page outside its own vector, which is
        // the number the reader is actually asking for.
        assert!(finding.verdict.contains("829"), "{finding:#?}");
        let remedy = finding.remedy.as_deref().expect("a remedy");
        assert!(
            remedy.contains("full-text"),
            "the remedy has to say what is *not* lost: {remedy}"
        );
    }

    /// Both faults at once is the interesting case, and the one a single
    /// finding would have flattened: a project can have a page the embedder
    /// refused and another it merely halved, and they share no remedy.
    #[test]
    fn a_failure_and_a_truncation_are_two_findings() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.embed_failures = vec![
            embed_failure("notes/a.md", "no model loaded"),
            embed_truncation("gotchas/long.md", 900),
        ];

        let found: Vec<Finding> = diagnose(&symptoms)
            .into_iter()
            .filter(|f| f.subject == "embeddings")
            .collect();

        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(found.iter().any(|f| f.severity == Severity::Broken));
        assert!(found.iter().any(|f| f.severity == Severity::Thin));
    }

    fn in_sections(mut failure: EmbedFailure, sections: usize) -> EmbedFailure {
        failure.sections = sections;
        failure
    }

    fn embeddings_finding(symptoms: &Symptoms) -> Option<Finding> {
        diagnose(symptoms)
            .into_iter()
            .find(|f| f.subject == "embeddings")
    }

    /// As retrieval ships, a query compares a long page's opening and not its
    /// sections, so a page holding sections is still thin. What changes is the
    /// remedy: the sections `anamnesis reindex` would add are already there,
    /// and recommending it would send somebody to rebuild an index for nothing.
    #[test]
    fn sections_retrieval_does_not_compare_leave_a_page_thin_without_sending_it_to_reindex() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sections_compared = false;
        symptoms.embed_failures = vec![in_sections(embed_truncation("gotchas/long.md", 3046), 51)];

        let finding = embeddings_finding(&symptoms).expect("still thin");

        assert_eq!(finding.severity, Severity::Thin);
        assert!(finding.verdict.contains("1 page"), "{finding:#?}");
        let remedy = finding.remedy.as_deref().expect("a remedy");
        assert!(remedy.contains("does not compare"), "{remedy}");
        assert!(
            !remedy.contains("reindex"),
            "the sections reindex adds are already there: {remedy}"
        );
    }

    /// Pages embedded before sections existed sit beside pages written after,
    /// and only the first kind is something a rebuild changes. The report is
    /// the one place that can tell them apart, because the count cannot.
    #[test]
    fn pages_with_and_without_sections_are_counted_apart() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sections_compared = false;
        symptoms.embed_failures = vec![
            in_sections(embed_truncation("sessions/new.md", 3046), 51),
            embed_truncation("sessions/old.md", 900),
            embed_truncation("sessions/older.md", 700),
        ];

        let finding = embeddings_finding(&symptoms).expect("finding");

        assert!(finding.verdict.contains("3 pages"), "{finding:#?}");
        let remedy = finding.remedy.as_deref().expect("a remedy");
        assert!(
            remedy.contains("1 of them is also embedded in sections"),
            "{remedy}"
        );
        assert!(remedy.contains("2 of them have no sections"), "{remedy}");
        assert!(remedy.contains("anamnesis reindex"), "{remedy}");
        assert!(
            remedy.contains(&MAX_SECTIONS.to_string()),
            "a page too long for sections is not one a rebuild fixes, and the remedy has to say so: {remedy}"
        );
    }

    /// Where retrieval compares sections, a page that has them is read in
    /// full — reporting it as thin would be describing a retrieval that does
    /// not run. The page without them still is, and its remedy is the rebuild.
    #[test]
    fn a_page_whose_sections_are_compared_is_not_thin() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.sections_compared = true;

        symptoms.embed_failures = vec![in_sections(embed_truncation("sessions/new.md", 3046), 51)];
        assert!(
            embeddings_finding(&symptoms).is_none(),
            "a page read in full is not thin"
        );

        symptoms.embed_failures = vec![
            in_sections(embed_truncation("sessions/new.md", 3046), 51),
            embed_truncation("sessions/old.md", 900),
        ];
        let finding = embeddings_finding(&symptoms).expect("the page without sections");
        assert!(finding.verdict.contains("1 page"), "{finding:#?}");
        assert!(finding.verdict.contains("sessions/old.md"), "{finding:#?}");
        assert!(
            !finding.verdict.contains("sessions/new.md"),
            "the worst page is the worst *thin* page: {finding:#?}"
        );
        let remedy = finding.remedy.as_deref().expect("a remedy");
        assert!(remedy.contains("It has no sections"), "{remedy}");
        assert!(!remedy.contains("does not compare"), "{remedy}");
    }

    /// Found on the live install the day it moved from MiniLM to
    /// nomic-embed-text: every page had a whole nomic vector, and doctor still
    /// reported four pages "embedded from 128 tokens", from MiniLM rows no
    /// query compares any more. A complaint is about the model it names, and
    /// only the model the server embeds with is the running system.
    #[test]
    fn complaints_under_a_model_the_server_no_longer_uses_are_not_judged() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.embed_failures = vec![embed_truncation("sessions/long.md", 905)];

        symptoms.server_embedding = Some("nomic-embed-text".to_owned());
        assert!(
            embeddings_finding(&symptoms).is_none(),
            "a MiniLM truncation says nothing about a server embedding with nomic"
        );

        symptoms.server_embedding = Some("all-MiniLM-L6-v2".to_owned());
        assert!(
            embeddings_finding(&symptoms).is_some(),
            "under the model that wrote it, it is still the fault it was"
        );

        symptoms.server_embedding = None;
        assert!(
            embeddings_finding(&symptoms).is_some(),
            "a server that did not say which model is not a reason to hide every row"
        );
    }

    /// The worst page, not the first one recorded. Somebody reading this is
    /// asking how bad it gets, and a list ordered by when it happened answers
    /// a different question.
    #[test]
    fn the_truncation_verdict_leads_with_the_worst_page() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.embed_failures = vec![
            embed_truncation("notes/slightly-long.md", 600),
            embed_truncation("gotchas/very-long.md", 2000),
            embed_truncation("notes/also-long.md", 700),
        ];

        let finding = diagnose(&symptoms)
            .into_iter()
            .find(|f| f.subject == "embeddings")
            .expect("finding");

        assert!(finding.verdict.contains("3 pages"), "{finding:#?}");
        assert!(
            finding.verdict.contains("gotchas/very-long.md"),
            "{finding:#?}"
        );
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
        let remedy = pages.remedy.as_ref().unwrap();
        assert!(remedy.contains("anamnesis key check"), "{pages:#?}");
        assert!(
            !remedy.contains("environment") && !pages.verdict.contains("inherit"),
            "the terminal's environment is not where the server's model comes from any more: {pages:#?}"
        );
    }

    /// What doctor said on 2026-09-15 with the key refused for a day: compare
    /// this terminal's environment with the server's. The server knew the
    /// reason; now the finding carries it, and the remedy is the one for it.
    #[test]
    fn a_refused_key_the_server_heard_is_the_reason_and_the_remedy() {
        let body = serde_json::json!({
            "consolidation": "gemini-3.5-flash",
            "consolidation_failure": {
                "at": "2026-09-14T21:47:35Z",
                "status": 400,
                "reason": "answered 400: Please pass a valid API key",
            },
        });
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.server_model_failure = model_failure(&body);
        symptoms.sessions = vec![SessionFacts {
            summary: Some(SummarySource::Counted),
            ..session(&[(EventKind::UserPrompt, 1), (EventKind::ToolUse, 3)])
        }];

        let findings = diagnose(&symptoms);
        let pages = findings
            .iter()
            .find(|f| f.subject == "pages")
            .expect("a pages finding");

        assert!(
            pages.verdict.ends_with(
                "; the server's model: gemini-3.5-flash answered 400: Please pass a valid API key"
            ),
            "{pages:#?}"
        );
        let remedy = pages.remedy.as_ref().unwrap();
        assert!(remedy.contains("key set ANAMNESIS_LLM_API_KEY"), "{remedy}");
        assert!(remedy.contains("restart the server"), "{remedy}");
    }

    /// Any other refusal is named, and the remedy is waiting for the model
    /// rather than replacing a key that works.
    #[test]
    fn another_refusal_is_named_without_blaming_the_key() {
        for reason in [
            "answered 503: The model is overloaded.",
            "answered 429: You exceeded your current quota. [GenerateRequestsPerDayPerProjectPerModel-FreeTier]",
            "answered 400: Invalid JSON payload received.",
        ] {
            assert!(!refuses_the_key(reason), "{reason}");
            let mut symptoms = wired("claude-code", &EVERY_MOMENT);
            symptoms.server_model_failure = Some(format!("gemini-3.5-flash {reason}"));
            symptoms.sessions = vec![SessionFacts {
                summary: Some(SummarySource::Counted),
                ..session(&[(EventKind::UserPrompt, 1)])
            }];
            let findings = diagnose(&symptoms);
            let pages = findings.iter().find(|f| f.subject == "pages").unwrap();
            assert!(pages.verdict.contains(reason), "{pages:#?}");
            assert!(
                !pages.remedy.as_ref().unwrap().contains("key set"),
                "{pages:#?}"
            );
        }
        for reason in [
            "answered 400: Invalid Auth key.",
            "answered 401: invalid x-api-key",
            "answered 403: Your API key was reported as leaked.",
        ] {
            assert!(refuses_the_key(reason), "{reason}");
        }
        assert_eq!(
            model_failure(
                &serde_json::json!({"consolidation": "m", "consolidation_failure": null})
            ),
            None
        );
        assert_eq!(model_failure(&serde_json::json!({"auth": "open"})), None);
    }

    /// A terminal with no provider exported says nothing about the server, so
    /// a project whose pages a model wrote is reported as healthy even though
    /// nothing here can see a model at all.
    #[test]
    fn a_shell_without_a_provider_is_not_evidence_against_the_server() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.server_model_failure = None;
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

    /// A key captured before its rule existed is the worst news doctor has,
    /// and the finding says what fired and how many, never the value.
    #[test]
    fn a_secret_left_in_stored_observations_is_exposed_and_first() {
        let mut symptoms = wired("claude-code", &EVERY_MOMENT);
        symptoms.stored_secrets.examined = 2000;
        symptoms.stored_secrets.count(&["google-auth-key"]);
        symptoms.stored_secrets.count(&["google-auth-key"]);

        let findings = diagnose(&symptoms);
        let first = findings.first().expect("a finding");
        assert_eq!(first.severity, Severity::Exposed);
        assert_eq!(first.subject, "secrets");
        assert!(
            first.verdict.contains("2 stored observation(s)"),
            "{}",
            first.verdict
        );
        assert!(
            first.verdict.contains("google-auth-key ×2"),
            "{}",
            first.verdict
        );
        assert!(
            first
                .remedy
                .as_deref()
                .is_some_and(|r| r.contains("redact --apply")),
            "{:?}",
            first.remedy
        );

        symptoms.stored_secrets = anamnesis_store::Redaction::default();
        assert!(
            diagnose(&symptoms).iter().all(|f| f.subject != "secrets"),
            "silent when nothing is stored"
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
        let symptoms = Symptoms::default();

        let findings = diagnose(&symptoms);

        assert_eq!(findings[0].severity, Severity::Broken);
        assert!(findings[0].verdict.contains("no harness"));
    }
}
