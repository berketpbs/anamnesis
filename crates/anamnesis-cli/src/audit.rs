//! Recording a deliberate change, and reading the record back.
//!
//! A command that runs against the data directory has no token and therefore
//! no operator: what it can honestly say is that somebody with the disk did
//! this, which is what [`Via::Cli`] means. The name is not invented — an audit
//! log that fills in "unknown" on every line is a log that has learned to
//! guess, and the one place it must not is the one where somebody is asking
//! why a page says what it says.
//!
//! Writing a line never fails the change it describes. Losing the record of a
//! deletion is bad; refusing to delete a page because the record could not be
//! written is worse, and the failure is said out loud instead.

use anamnesis_core::audit::{Action, AuditEntry, Via};
use anamnesis_core::ids::ProjectId;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::format::describe_age;
use crate::project::open_project;

/// Record one change made from the command line.
///
/// `project` is `None` for a change that belongs to no single project — a
/// restore replaces the whole data directory — and a nil identifier would be
/// this log's own kind of lie about that.
pub fn note(
    store: &Store,
    project: Option<ProjectId>,
    action: Action,
    subject: impl Into<String>,
    detail: Option<String>,
) {
    let mut entry = AuditEntry::new(action, Via::Cli, subject, Timestamp::now());
    if let Some(project) = project {
        entry = entry.in_project(project);
    }
    if let Some(detail) = detail {
        entry = entry.saying(detail);
    }
    if let Err(error) = store.append_audit(&entry) {
        // stderr, and never a failure: the change has already happened, and
        // the only thing left to decide is whether anybody hears about the
        // gap in the log.
        eprintln!("anamnesis: the change was made but not recorded in the audit log: {error}");
    }
}

/// Show what has been changed, newest first.
pub fn cmd_audit(
    limit: usize,
    everywhere: bool,
    data_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;
    let project = if everywhere {
        None
    } else {
        Some(scope.project_id)
    };
    let trail = store.audit_trail(project, limit)?;
    let now = Timestamp::now();

    println!(
        "📓 Changes to {}",
        if everywhere {
            "every project on this machine".to_owned()
        } else {
            scope.scope.to_string()
        }
    );
    println!();

    if trail.is_empty() {
        // Two different silences, and sending somebody to look for a bug in
        // the first case would waste an afternoon.
        if store.audit_len()? == 0 {
            println!("  Nothing has been changed by hand yet.");
            println!();
            println!("  Capture is recorded as sessions, not here — this is the log of");
            println!("  deliberate changes: pages written or forgotten, sessions removed,");
            println!("  handoffs claimed, proposals carried out.");
        } else {
            println!("  Nothing in this project. `anamnesis audit --everywhere` reads the rest.");
        }
        return Ok(());
    }

    for entry in &trail {
        println!("  {:>8}  {}", describe_age(entry.at, now), entry.summary());
    }
    println!();
    println!("  {} change(s) shown.", trail.len());
    Ok(())
}
