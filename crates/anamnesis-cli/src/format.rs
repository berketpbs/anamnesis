//! The small phrasings every command shares.
//!
//! Three of them, and they are here rather than beside any one caller because
//! `status`, `init`, `reindex` and the proposal list all say the same things
//! about counts, ages, and where a project's identity came from. A sentence
//! that reads one way in one command and another way in the next is how a
//! person learns to distrust all of them.

use anamnesis_core::scope::ScopeSource;
use jiff::Timestamp;

/// `1 page` / `2 pages`, so a count never has to be read as `1 page(s)`.
pub fn plural(count: i64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// How long ago something happened, in the roughest useful unit.
pub fn describe_age(then: Timestamp, now: Timestamp) -> String {
    let minutes = (now.as_millisecond() - then.as_millisecond()) / 60_000;
    match minutes {
        ..1 => "just now".to_owned(),
        1..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

/// Explain, in one line, how the project was identified.
///
/// A wrong scope is the likeliest reason memory appears empty, so this is the
/// first thing `status` should make visible.
pub fn describe_source(source: &ScopeSource) -> String {
    match source {
        ScopeSource::Marker { path, legacy } => {
            let name = if *legacy { "legacy marker" } else { "marker" };
            format!("pinned by {name} at {}", path.display())
        }
        ScopeSource::GitRemote { normalized } => format!("git remote {normalized}"),
        ScopeSource::GitRoot { path } => format!("git working tree {}", path.display()),
        ScopeSource::CwdBasename { path } => {
            format!("directory name {}", path.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_pluralised_rather_than_parenthesised() {
        assert_eq!(plural(0, "page"), "0 pages");
        assert_eq!(plural(1, "page"), "1 page");
        assert_eq!(plural(2, "page"), "2 pages");
    }
}
