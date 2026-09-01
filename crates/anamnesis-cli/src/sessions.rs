//! Sessions, and the note one leaves for the next.
//!
//! A handoff is the whole point of the system from the outside: it is what a
//! new agent is handed when it starts. Everything here is about that object's
//! lifetime — which slot it belongs to, who is allowed to take it, what is
//! left when nobody does, and how to remove a session that should never have
//! been one.

use std::path::{Path, PathBuf};

use anamnesis_store::{RawSpool, SessionSummary, Store};

use anamnesis_core::audit::Action;

use crate::audit::note;
use crate::project::open_project;

/// Show the handoff waiting for the next session, without consuming it.
///
/// Deliberately a peek, not a claim: running this to see what is waiting must
/// not be the reason the next agent session starts with nothing.
/// Show, claim, or discard the note this project's last session left.
pub fn cmd_handoff(
    workstream: Option<String>,
    operator: Option<String>,
    discard: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;

    let workstream_id = match workstream.as_deref().map(str::trim) {
        Some(slug) => Some(
            store
                .find_workstream(scope.project_id, slug)?
                .ok_or_else(|| anyhow::anyhow!("no workstream named {slug:?}"))?
                .id,
        ),
        None => None,
    };

    // Refused rather than ignored: someone who passes `--operator` on a
    // project that keys one slot is asking a question about a separation that
    // does not exist, and answering with the shared slot would look like an
    // answer about theirs.
    if operator.is_some() && !scope.slots.per_user {
        anyhow::bail!(
            "this project keeps one handoff slot; --operator needs `[slots] per_user = true` in {}",
            scope
                .marker
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| anamnesis_core::scope::MARKER_FILE.to_owned())
        );
    }

    let operator = match operator.as_deref().map(str::trim) {
        Some(name) => Some(anamnesis_core::scope::OperatorName::parse(name)?),
        None => None,
    };

    let slot =
        anamnesis_core::handoff::Slot::for_workstream(workstream_id).for_operator(operator.clone());

    if discard {
        // Read back rather than reported as a bare success: a note is being
        // thrown away, and the person doing it should see what it said in case
        // it was not the one they meant.
        return match store.discard_handoff(scope.project_id, &slot)? {
            Some(body) => {
                note(
                    &store,
                    Some(scope.project_id),
                    Action::HandoffDiscarded,
                    describe_slot(&workstream, &operator)
                        .trim()
                        .trim_start_matches("for ")
                        .to_owned(),
                    Some(format!("{} character(s) thrown away", body.len())),
                );
                println!(
                    "🗑  Discarded the handoff{}:",
                    describe_slot(&workstream, &operator)
                );
                println!();
                println!("{body}");
                println!();
                println!("  The next session will start without one. The row is kept,");
                println!("  marked expired, so what was written is still on record.");
                Ok(())
            }
            None => {
                println!(
                    "Nothing waiting{} — nothing to discard.",
                    describe_slot(&workstream, &operator)
                );
                Ok(())
            }
        };
    }

    match store.peek_handoff(scope.project_id, &slot)? {
        Some(body) => {
            println!(
                "📋 Pending handoff{}:",
                describe_slot(&workstream, &operator)
            );
            println!();
            println!("{body}");
        }
        None => println!(
            "Nothing waiting{} — the last session left no handoff, or it was already claimed.",
            describe_slot(&workstream, &operator)
        ),
    }
    Ok(())
}

/// Name the slot that was looked in, when it was not the only one.
///
/// Said every time it is not the shared slot, because "nothing waiting" and
/// "nothing waiting in this one slot of several" are different answers, and
/// the second one is the one that sends someone looking in the right place.
fn describe_slot(
    workstream: &Option<String>,
    operator: &Option<anamnesis_core::scope::OperatorName>,
) -> String {
    match (workstream, operator) {
        (None, None) => String::new(),
        (Some(slug), None) => format!(" for workstream {slug}"),
        (None, Some(operator)) => format!(" for {operator}"),
        (Some(slug), Some(operator)) => format!(" for {operator} in workstream {slug}"),
    }
}

/// List the sessions this project has recorded, newest first.
pub fn cmd_sessions(limit: Option<usize>, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;
    let sessions = store.recent_sessions(scope.project_id, limit.unwrap_or(20))?;

    if sessions.is_empty() {
        println!("No sessions recorded for {}.", scope.scope);
        return Ok(());
    }

    for session in &sessions {
        let short: String = session.id.to_string().chars().take(8).collect();
        let when = session.started_at.to_string();
        let when = when.split('.').next().unwrap_or(&when);
        // Both are absent for the ordinary session, and both change what the
        // line means when they are not: which thread of work it belongs to,
        // and whose it was.
        let workstream = match &session.workstream {
            Some(slug) => format!(" · {slug}"),
            None => String::new(),
        };
        let operator = match &session.operator {
            Some(operator) => format!(" · {operator}"),
            None => String::new(),
        };
        println!(
            "{short}  {when}  {:<12} {:<7} {} obs{workstream}{operator}",
            session.agent, session.state, session.observation_count
        );
    }
    Ok(())
}

/// Resolve an id prefix to exactly one session.
///
/// Refuses an ambiguous prefix rather than acting on whichever row sorted
/// first, for the reason `one_proposal` does: what follows cannot be undone,
/// and "it removed the other one" is not a mistake anybody can walk back.
fn one_session(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
) -> anyhow::Result<SessionSummary> {
    // An empty prefix abbreviates every session in the project, which is the
    // one argument this command must never accept.
    if prefix.trim().is_empty() {
        anyhow::bail!("name a session — an empty prefix matches every session in the project");
    }

    let mut matches = store.sessions_matching(scope.project_id, prefix)?;
    match matches.len() {
        0 => anyhow::bail!("no session in {} starts with {prefix:?}", scope.scope),
        1 => Ok(matches.remove(0)),
        _ => {
            let listed: Vec<String> = matches
                .iter()
                .map(|session| {
                    let short: String = session.id.to_string().chars().take(8).collect();
                    format!("{short} ({} obs)", session.observation_count)
                })
                .collect();
            anyhow::bail!(
                "{prefix:?} matches {}: {}",
                matches.len(),
                listed.join(", ")
            )
        }
    }
}

/// Remove a session somebody named, from the index and the raw spool.
///
/// The counterpart to `forget` for the other half of what memory holds. What
/// it exists for is not a session that went badly — once that has become a
/// page, `forget` and the sweep both reach it — but a session that was never
/// a session. A hook fired by hand to check whether capture is alive records
/// a session indistinguishable from somebody's afternoon, and it is
/// permanent: it is counted in `status`, it is listed by `sessions`, and once
/// it has been silent long enough it is summarised into a page like any
/// other.
///
/// Gated behind `--apply`, unlike `forget`. The reasoning there rests on the
/// wiki being a git repository, so a page removed by mistake is still in its
/// history. Nothing plays that part here: the transcript under `raw/` is the
/// only copy of what a session observed and it is not versioned. Where there
/// is no history to fall back on, the report before the fact is the whole
/// safety net.
/// Forget sessions outright: their observations and their transcripts.
pub fn cmd_forget_session(
    prefixes: &[String],
    apply: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let raw = RawSpool::new(data.raw());

    // Every prefix is resolved before anything is removed, for the reason
    // `forget` resolves every path first: refusing the third name after
    // acting on the first two leaves the caller to work out which was the
    // typo, against a listing that has changed underneath them.
    let mut doomed: Vec<(SessionSummary, Vec<PathBuf>)> = Vec::with_capacity(prefixes.len());
    for prefix in prefixes {
        let session = one_session(&store, &scope, prefix)?;
        // Two prefixes for one session is a typo that costs nothing, so it is
        // absorbed rather than refused.
        if doomed.iter().any(|(seen, _)| seen.id == session.id) {
            continue;
        }
        // Every file that carries this session, not only the one its start
        // date names. A session recorded before the capture path wrote its
        // header from the stored session was filed under two dates when it ran
        // past midnight, and forgetting only the reachable one would leave the
        // other on disk after somebody asked for it to be gone.
        let transcripts = raw.locate_all(&scope.scope, session.id);
        doomed.push((session, transcripts));
    }

    println!("🗑  Forgetting from {}", scope.scope);
    println!();
    let mut observations = 0;
    for (session, transcripts) in &doomed {
        observations += session.observation_count;
        let short: String = session.id.to_string().chars().take(8).collect();
        let when = session.started_at.to_string();
        let when = when.split('.').next().unwrap_or(&when).to_owned();
        println!(
            "  {short}  {when}  {:<12} {:<7} {} obs",
            session.agent, session.state, session.observation_count
        );
        if transcripts.is_empty() {
            println!("     transcript  none on disk");
        }
        for transcript in transcripts {
            match lines_in(transcript) {
                Some(lines) => {
                    println!("     transcript  {} ({lines} lines)", transcript.display())
                }
                None => println!("     transcript  {}", transcript.display()),
            }
        }
    }
    println!();

    if !apply {
        println!("  Nothing has been removed. Run again with --apply to carry this out.");
        return Ok(());
    }

    // The index first, then the spool — the order `forget` and `sweep` both
    // chose, and here it is what keeps an interruption harmless: a session
    // whose row is gone while its transcript remains is restored whole by
    // `anamnesis reindex`, because that is where reindex rebuilds sessions
    // from. The reverse order destroys the durable copy while the index still
    // claims the session, and nothing rebuilds that.
    let mut rows = 0;
    let mut transcripts = 0;
    for (session, files) in &doomed {
        if store.delete_session(session.id)? {
            rows += 1;
        }
        note(
            &store,
            Some(scope.project_id),
            Action::SessionForgotten,
            session.id.to_string(),
            Some(format!("{} observation(s)", session.observation_count)),
        );
        for transcript in files {
            match std::fs::remove_file(transcript) {
                Ok(()) => transcripts += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => anyhow::bail!(
                    "{} index row(s) were removed, but the transcript at {} could not be: {error}",
                    rows,
                    transcript.display()
                ),
            }
        }
    }

    println!("  {rows} session(s), {observations} observation(s), {transcripts} transcript(s).");
    println!();
    println!("  Not recoverable: raw/ is not a git repository.");
    println!(
        "  Any page a session already produced stays in the wiki — `anamnesis forget` removes those."
    );
    Ok(())
}

/// How many lines a transcript holds, or `None` if it is not there.
///
/// Counted rather than sized because the unit a person can check against is
/// the one `sessions` already prints: a session header and one line per
/// observation.
fn lines_in(path: &Path) -> Option<usize> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.lines().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_operator(name: &str) -> anamnesis_core::scope::OperatorName {
        anamnesis_core::scope::OperatorName::parse(name).expect("valid operator")
    }

    #[test]
    fn a_peeked_slot_is_named_only_when_it_is_not_the_only_one() {
        assert_eq!(describe_slot(&None, &None), "");
        assert_eq!(
            describe_slot(&Some("auth".to_owned()), &None),
            " for workstream auth"
        );
        assert_eq!(
            describe_slot(&None, &Some(an_operator("alice"))),
            " for alice"
        );
        assert_eq!(
            describe_slot(&Some("auth".to_owned()), &Some(an_operator("alice"))),
            " for alice in workstream auth"
        );
    }
}
