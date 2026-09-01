//! `anamnesis rename`: the same memory, under a name that resolves.
//!
//! A project's identity is derived — from its workspace and the key its name
//! or git remote produces — and every page's identity is derived from that.
//! Two clones of one repository therefore share one memory without configuring
//! anything, which is the point. The cost shows up the day a repository is
//! renamed or moved to a new remote: the next session resolves a different
//! key, finds an empty project, and nothing says the old memory is still there
//! under a name nobody types any more.
//!
//! This moves it. Four things travel together and the order is the same
//! principle every destructive command here follows — the recoverable first,
//! the irreplaceable last:
//!
//! 1. the wiki's pages, as one git commit that reads as a move;
//! 2. the index, in one transaction, with every derived identifier
//!    recomputed;
//! 3. the transcripts, which are a directory rename and nothing else;
//! 4. the marker file, so that the next session resolves the new name rather
//!    than re-deriving the old one and finding an empty project again.
//!
//! Step four is the one that makes the other three stick. Without it the very
//! next event would create the old project over again, and the rename would
//! read as having quietly failed.

use std::path::PathBuf;

use anamnesis_core::audit::Action;
use anamnesis_core::ids::ProjectId;
use anamnesis_core::scope::{ProjectKey, ProjectName, Scope};
use jiff::Timestamp;

use crate::audit::note;
use crate::project::open_project;

/// Move this project's memory to a new name.
pub fn cmd_rename(new_name: &str, apply: bool, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let name = ProjectName::sanitized(new_name)?;
    let key = ProjectKey::from_name(&name);
    let target = ProjectId::derive(&scope.scope.workspace, &key);
    let renamed = Scope {
        workspace: scope.scope.workspace.clone(),
        project: name.clone(),
    };

    if target == scope.project_id {
        println!("{} is already called {name}.", scope.scope);
        return Ok(());
    }

    let counted = store.purge_preview(scope.project_id)?;
    let wiki = anamnesis_wiki::Wiki::open(data.wiki())?;
    let pages = wiki.pages(&scope.scope)?;
    let transcripts = transcripts_of(&data, &scope.scope);
    let marker = scope.marker.clone();

    println!("🔁 Renaming {} → {renamed}", scope.scope);
    println!();
    println!(
        "  pages       {} in the index, {} on disk",
        counted.pages,
        pages.len()
    );
    println!(
        "  sessions    {} with {} observation(s)",
        counted.sessions, counted.observations
    );
    println!("  wiki        {}", wiki.scope_root(&scope.scope).display());
    println!("              → {}", wiki.scope_root(&renamed).display());
    if transcripts.exists() {
        println!("  transcripts {}", transcripts.display());
        println!(
            "              → {}",
            transcripts_of(&data, &renamed).display()
        );
    }
    match &marker {
        Some(path) => println!("  marker      {} (project = \"{name}\")", path.display()),
        None => println!("  marker      none — one will be written to pin the new name"),
    }

    if !apply {
        println!();
        println!("  Nothing has been changed. Run again with --apply to carry this out.");
        println!();
        println!("  Every page's identifier is derived from the project's, so this is a");
        println!("  migration rather than a rename: the index moves in one transaction,");
        println!("  and either all of it arrives or none of it does.");
        return Ok(());
    }

    // The wiki first, as one commit that reads as a move: its history is what
    // stands behind a page once the working tree has changed.
    let commit = wiki.move_scope(
        &scope.scope,
        &renamed,
        &format!("rename: {} → {renamed}", scope.scope),
    )?;

    // Then the index, in one transaction, with every derived identifier
    // recomputed.
    let moved = store.rename_project(scope.project_id, target, &name, &key, Timestamp::now())?;

    // Then the transcripts, which are only ever a directory.
    if transcripts.exists() {
        let destination = transcripts_of(&data, &renamed);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&transcripts, &destination)?;
    }

    // And last, the thing that makes it stick: without this the next event
    // re-derives the old identity and creates the old project over again.
    let marker_path = marker.unwrap_or_else(|| scope.root.join(".anamnesis.toml"));
    pin_project(&marker_path, &name)?;

    note(
        &store,
        Some(target),
        Action::Renamed,
        format!("{} → {renamed}", scope.scope),
        Some(format!(
            "{} page(s), {} session(s), {} audit line(s)",
            moved.pages, moved.sessions, moved.audit_lines
        )),
    );

    println!();
    println!("  Moved.");
    println!(
        "  {} page(s), {} session(s), {} audit line(s)",
        moved.pages, moved.sessions, moved.audit_lines
    );
    match &commit {
        Some(commit) => println!("  commit {}", &commit[..commit.len().min(8)]),
        None => println!("  the wiki had nothing to record"),
    }
    println!("  marker {}", marker_path.display());
    println!();
    println!("  The next session will resolve {renamed}. Nothing else has to change.");
    Ok(())
}

/// Where a scope's transcripts live.
fn transcripts_of(data: &anamnesis_core::datadir::DataDir, scope: &Scope) -> PathBuf {
    data.raw()
        .join(scope.workspace.as_str())
        .join(scope.project.as_str())
}

/// Write `project = "<name>"` into the marker file, keeping everything else.
///
/// Through `toml_edit` rather than by re-serialising: a marker file carries
/// comments explaining every setting in it — this repository's own does, at
/// length — and a rename that reformatted them would be taking more than it
/// was asked for.
fn pin_project(path: &std::path::Path, name: &ProjectName) -> anyhow::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut document: toml_edit::DocumentMut = existing.parse()?;
    // A marker written here has to look like one somebody wrote: a `[scope]`
    // header, not the inline table `toml_edit` produces by default when it has
    // to invent the key. This file is read by people far more often than by
    // anything else.
    if document.get("scope").is_none() {
        let mut table = toml_edit::Table::new();
        table.set_implicit(false);
        document["scope"] = toml_edit::Item::Table(table);
    }
    document["scope"]["project"] = toml_edit::value(name.as_str());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, document.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker is somebody's file, and this repository's own carries fifty
    /// lines of comment explaining every setting in it.
    #[test]
    fn pinning_the_name_keeps_the_rest_of_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".anamnesis.toml");
        std::fs::write(
            &path,
            "# what this file is for\n[scope]\nworkspace = \"default\"\nproject = \"widget\"\n\n# what the sweep forgets\n[decay]\nthreshold = 0.05\n",
        )
        .expect("marker");

        pin_project(&path, &ProjectName::sanitized("gadget").expect("name")).expect("pin");

        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("project = \"gadget\""), "{written}");
        assert!(written.contains("# what this file is for"), "{written}");
        assert!(written.contains("# what the sweep forgets"), "{written}");
        assert!(written.contains("threshold = 0.05"), "{written}");
        assert!(written.contains("workspace = \"default\""), "{written}");
    }

    /// A project with no marker file gets one, because the pin is the whole
    /// reason the rename holds: without it the next event re-derives the old
    /// identity from the git remote and creates the old project again.
    #[test]
    fn a_project_without_a_marker_gets_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".anamnesis.toml");

        pin_project(&path, &ProjectName::sanitized("gadget").expect("name")).expect("pin");

        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("[scope]"), "{written}");
        assert!(written.contains("project = \"gadget\""), "{written}");
    }
}
