//! Stamps the build with the commit it was built from.
//!
//! `1.0.0` does not move between commits, so a version string alone cannot
//! answer the question that matters in practice: is the binary that is
//! recording my sessions the one I built this afternoon, or the one from three
//! weeks ago that predates half of what the pages are supposed to say? On this
//! project that gap went unnoticed for as long as it existed — the setup looked
//! healthy from every angle, and the only symptom was pages that were quietly
//! worth less.
//!
//! Everything here degrades to `unknown` rather than failing. A build from a
//! source archive, a vendored copy, or a Docker context without `.git` is a
//! legitimate build; refusing to compile it to protect a diagnostic would be
//! the tail wagging the dog.

use std::process::Command;

fn main() {
    let commit = commit().unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=ANAMNESIS_BUILD_COMMIT={commit}");

    // Without these, a rebuild after a commit keeps the stamp of the one
    // before it — which is worse than no stamp, because it is a wrong answer
    // to the exact question this exists to answer.
    for path in [".git/HEAD", ".git/refs/heads"] {
        println!("cargo:rerun-if-changed=../../{path}");
    }
}

/// The short commit hash, and a `+` when the tree had uncommitted changes.
fn commit() -> Option<String> {
    let hash = run(&["rev-parse", "--short=7", "HEAD"])?;
    let dirty = run(&["status", "--porcelain"]).is_some_and(|out| !out.is_empty());
    Some(if dirty { format!("{hash}+") } else { hash })
}

/// Run a git command, or give up quietly.
fn run(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_owned())
}
