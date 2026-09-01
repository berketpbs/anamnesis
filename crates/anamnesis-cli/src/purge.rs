//! `anamnesis purge`: this project's memory, all of it.
//!
//! The end of the family `forget` and `forget-session` start. It is for the
//! memory that is wrong rather than incomplete — a repository re-scoped by
//! accident, a `bootstrap` run against the wrong directory, a project that was
//! never meant to be remembered — where fixing it page by page is worse than
//! starting the project again.
//!
//! **Nothing happens without `--apply`.** The same rule `forget`, `sweep` and
//! `restore` follow, and here it carries the most: this is the one command
//! that removes an entire project's transcripts, and those are the half of
//! memory nothing rebuilds.
//!
//! The order matters and it is chosen by what can be got back. Pages go first,
//! as a git commit, so they stay in the wiki's history. The index goes second,
//! because it is rebuildable from the wiki and the transcripts. The
//! transcripts go last, because when they are gone they are gone — and an
//! interruption anywhere before that leaves the only irreplaceable part
//! standing.

use std::path::PathBuf;

use anamnesis_core::audit::Action;
use anamnesis_store::Purged;

use crate::audit::note;
use crate::project::open_project;

/// Remove one project's memory, after saying exactly what that means.
pub fn cmd_purge(apply: bool, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let wiki = anamnesis_wiki::Wiki::open(data.wiki())?;

    let counted = store.purge_preview(scope.project_id)?;
    let pages = wiki.pages(&scope.scope)?;
    let transcripts = data
        .raw()
        .join(scope.scope.workspace.as_str())
        .join(scope.scope.project.as_str());
    let transcript_files = count_files(&transcripts);

    println!("🔥 Purging {}", scope.scope);
    println!();
    report(&counted, pages.len(), transcript_files);
    println!();
    println!("  wiki        {}", wiki.scope_root(&scope.scope).display());
    println!("  transcripts {}", transcripts.display());

    if counted.is_empty() && pages.is_empty() && transcript_files == 0 {
        println!();
        println!("  There is nothing here to remove.");
        return Ok(());
    }

    if !apply {
        println!();
        println!("  Nothing has been removed. Run again with --apply to carry this out.");
        println!();
        println!("  Pages come back from the wiki's git history. Transcripts do not:");
        println!("  `raw/` is not a repository, and it is the only copy of what was");
        println!("  said in those sessions. `anamnesis backup` first, if in doubt.");
        return Ok(());
    }

    // Pages first, and as a commit: what leaves this way is still in the
    // wiki's history, which is the only reason a purge is survivable at all.
    let commit = if pages.is_empty() {
        None
    } else {
        wiki.delete_pages(
            &scope.scope,
            &pages,
            &format!("purge: {} ({} page(s))", scope.scope, pages.len()),
        )?
    };

    // Then the index, which is rebuildable from what is left until the next
    // step removes that too.
    let purged = store.purge_project(scope.project_id)?;

    // Last, because this is the part nothing rebuilds.
    let removed_transcripts = if transcripts.exists() {
        std::fs::remove_dir_all(&transcripts)?;
        transcript_files
    } else {
        0
    };

    // Written after, and it survives: `audit_log` has no foreign key to the
    // project, so the line saying where this memory went outlives the memory.
    note(
        &store,
        Some(scope.project_id),
        Action::Purged,
        scope.scope.to_string(),
        Some(format!(
            "{} page(s), {} session(s), {} observation(s), {removed_transcripts} transcript(s)",
            purged.pages, purged.sessions, purged.observations
        )),
    );

    println!();
    println!("  Removed.");
    match &commit {
        Some(commit) => {
            println!("  commit {}", &commit[..commit.len().min(8)]);
            println!();
            println!("  The pages are still in the wiki's history:");
            println!("    git -C {} show {commit}", data.wiki().display());
        }
        None => println!("  The wiki had nothing to record."),
    }
    println!();
    println!("  The transcripts are not recoverable, and neither is the index");
    println!("  built from them. `anamnesis audit` still says this happened.");
    Ok(())
}

/// What is about to go, or what went.
fn report(counted: &Purged, pages_on_disk: usize, transcripts: usize) {
    println!(
        "  pages       {} in the index, {pages_on_disk} on disk",
        counted.pages
    );
    println!(
        "  sessions    {} with {} observation(s)",
        counted.sessions, counted.observations
    );
    println!("  transcripts {transcripts} file(s)");
    if counted.handoffs > 0 || counted.workstreams > 0 || counted.proposals > 0 {
        println!(
            "  also        {} handoff(s), {} workstream(s), {} proposal(s)",
            counted.handoffs, counted.workstreams, counted.proposals
        );
    }
}

/// How many transcript files a project has, however deep the date directories.
fn count_files(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => count_files(&entry.path()),
            Ok(_) => 1,
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcripts_are_counted_through_the_date_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("default").join("widget");
        std::fs::create_dir_all(root.join("2026-09-01")).expect("dirs");
        std::fs::create_dir_all(root.join("2026-09-02")).expect("dirs");
        std::fs::write(root.join("2026-09-01/a.jsonl"), "{}").expect("file");
        std::fs::write(root.join("2026-09-01/b.jsonl"), "{}").expect("file");
        std::fs::write(root.join("2026-09-02/c.jsonl"), "{}").expect("file");

        assert_eq!(count_files(&root), 3);
        assert_eq!(count_files(&dir.path().join("nowhere")), 0);
    }
}
