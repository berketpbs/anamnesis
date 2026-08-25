//! Capture exclusions: the paths a project has asked never to be remembered.
//!
//! `[capture] ignore_paths` in the marker file names paths whose events must
//! not be recorded at all — not redacted, not truncated, not written and then
//! forgotten. A build directory is noise; `.env` is worse than noise.
//!
//! This is a second line, never the first. Redaction runs on every observation
//! regardless, because a secret pasted into a prompt has no path to exclude.
//! What exclusion adds is the ability to say "events about this file are not
//! interesting, or not mine to keep", which redaction cannot express.
//!
//! Two deliberate choices in the matching:
//!
//! * **Gitignore-shaped patterns**, because that is the syntax anyone writing
//!   `target/**` already has in their fingers. A pattern with no slash matches
//!   at any depth (`*.log` catches `logs/app.log`), and a pattern ending in a
//!   slash matches everything beneath it (`target/` becomes `target/**`).
//! * **Case-insensitive everywhere**, including on Linux where the filesystem
//!   is not. An exclusion that silently fails because someone wrote `.ENV` in
//!   one place and `.env` in another is a worse outcome than one that excludes
//!   slightly more than it was asked to.
//!
//! A path is tested twice: as given, and relative to the project root. So both
//! `target/**` and `/home/me/project/target/**` exclude the same file, and
//! neither the agent's absolute paths nor the user's relative patterns have to
//! know about the other.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::config::CaptureConfig;
use crate::error::{CoreError, Result};

/// Compiled `[capture] ignore_paths` patterns, ready to test paths against.
#[derive(Debug, Clone)]
pub struct CaptureFilter {
    set: Option<GlobSet>,
    root: PathBuf,
}

impl CaptureFilter {
    /// Compile a project's exclusions.
    ///
    /// `root` is what relative patterns are relative to — the directory
    /// holding the marker file, or the repository root when there is no
    /// marker.
    pub fn compile(config: &CaptureConfig, root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if config.ignore_paths.is_empty() {
            return Ok(Self { set: None, root });
        }

        let mut builder = GlobSetBuilder::new();
        for pattern in &config.ignore_paths {
            builder.add(compile_one(pattern)?);
        }

        let set = builder.build().map_err(|error| CoreError::InvalidName {
            kind: "ignore_paths",
            value: config.ignore_paths.join(", "),
            reason: leak(error.to_string()),
        })?;

        Ok(Self {
            set: Some(set),
            root,
        })
    }

    /// A filter that excludes nothing, for callers with no marker file.
    pub fn allow_all(root: impl Into<PathBuf>) -> Self {
        Self {
            set: None,
            root: root.into(),
        }
    }

    /// Whether any pattern was configured.
    pub fn is_empty(&self) -> bool {
        self.set.is_none()
    }

    /// Whether events about this path should be dropped.
    pub fn excludes(&self, path: &str) -> bool {
        let Some(set) = &self.set else {
            return false;
        };

        let candidate = normalize(path);
        if set.is_match(&candidate) {
            return true;
        }

        // Also test the path as the project sees it, so `target/**` written in
        // a marker file matches the absolute path an agent reports.
        match relative_to(&self.root, &candidate) {
            Some(relative) => set.is_match(relative),
            None => false,
        }
    }

    /// The first excluded path among `paths`, if any.
    ///
    /// One match is enough to drop the whole event: an observation that
    /// mentions two files carries the contents of both, so keeping it because
    /// only one of them was excluded would defeat the exclusion.
    pub fn first_excluded<'a, I>(&self, paths: I) -> Option<&'a str>
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.set.as_ref()?;
        paths.into_iter().find(|path| self.excludes(path))
    }

    /// Directory relative patterns resolve against.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Turn one configured pattern into a glob, applying the gitignore shorthands.
fn compile_one(pattern: &str) -> Result<Glob> {
    let trimmed = pattern.trim();
    let invalid = |reason: &'static str| CoreError::InvalidName {
        kind: "ignore_paths",
        value: pattern.to_owned(),
        reason,
    };
    if trimmed.is_empty() {
        return Err(invalid("must not be empty"));
    }

    let normalized = normalize(trimmed);
    let expanded = if let Some(stripped) = normalized.strip_suffix('/') {
        // `target/` means the directory and everything in it.
        format!("{stripped}/**")
    } else if normalized.contains('/') {
        normalized
    } else {
        // No slash: match at any depth, the way `.gitignore` does.
        format!("**/{normalized}")
    };

    // `literal_separator` is what keeps `*` from crossing a `/`, so `*.log`
    // means what it does in a `.gitignore` rather than matching half a path.
    GlobBuilder::new(&expanded)
        .case_insensitive(true)
        .literal_separator(true)
        .build()
        .map_err(|error| CoreError::InvalidName {
            kind: "ignore_paths",
            value: pattern.to_owned(),
            reason: leak(error.to_string()),
        })
}

/// Separators as globs understand them, and no trailing noise.
fn normalize(path: &str) -> String {
    path.trim().replace('\\', "/")
}

/// The part of `path` below `root`, when it is below it at all.
///
/// Compared case-insensitively for the same reason the globs are: a project
/// at `C:\Repo` reporting `c:\repo\.env` must still match `.env`.
fn relative_to<'a>(root: &Path, path: &'a str) -> Option<&'a str> {
    let root = normalize(&root.to_string_lossy());
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return None;
    }

    let lower_path = path.to_lowercase();
    let lower_root = root.to_lowercase();
    let rest = lower_path.strip_prefix(&lower_root)?;
    if !rest.starts_with('/') {
        return None;
    }
    Some(&path[root.len() + 1..])
}

/// Keep a message alive for an error type that holds `&'static str`.
///
/// Only reachable on a malformed pattern, which happens once per process at
/// configuration load, so the leak is bounded by the number of typos in a
/// marker file.
fn leak(message: String) -> &'static str {
    Box::leak(message.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(patterns: &[&str], root: &str) -> CaptureFilter {
        let config = CaptureConfig {
            ignore_paths: patterns.iter().map(|p| (*p).to_owned()).collect(),
        };
        CaptureFilter::compile(&config, root).expect("compile")
    }

    #[test]
    fn no_patterns_excludes_nothing() {
        let filter = filter(&[], "/repo");

        assert!(filter.is_empty());
        assert!(!filter.excludes("/repo/.env"));
    }

    #[test]
    fn a_directory_pattern_covers_everything_beneath_it() {
        let filter = filter(&["target/**"], "/repo");

        assert!(filter.excludes("target/debug/app"));
        assert!(filter.excludes("/repo/target/debug/app"));
        assert!(!filter.excludes("/repo/src/target.rs"));
    }

    #[test]
    fn a_trailing_slash_means_the_whole_directory() {
        // `target/` is what people write; without the expansion it would match
        // nothing at all, which is the worst possible failure for an exclusion.
        let filter = filter(&["target/"], "/repo");

        assert!(filter.excludes("/repo/target/debug/app"));
    }

    #[test]
    fn a_pattern_without_a_slash_matches_at_any_depth() {
        let filter = filter(&["*.log", ".env"], "/repo");

        assert!(filter.excludes("/repo/app.log"));
        assert!(filter.excludes("/repo/logs/nested/app.log"));
        assert!(filter.excludes("/repo/config/.env"));
        assert!(!filter.excludes("/repo/src/env.rs"));
    }

    #[test]
    fn matching_ignores_case_and_separator_style() {
        // Windows reports backslashes and does not care about case; a secret
        // must not survive because of either.
        let filter = filter(&[".env"], r"C:\Repo");

        assert!(filter.excludes(r"C:\Repo\.ENV"));
        assert!(filter.excludes(r"c:\repo\config\.env"));
    }

    #[test]
    fn one_excluded_path_condemns_the_whole_event() {
        let filter = filter(&[".env"], "/repo");

        let hit = filter.first_excluded(vec!["/repo/src/lib.rs", "/repo/.env"]);

        assert_eq!(hit, Some("/repo/.env"));
    }

    #[test]
    fn a_malformed_pattern_is_reported_rather_than_ignored() {
        let config = CaptureConfig {
            ignore_paths: vec!["src/**/[".to_owned()],
        };

        assert!(CaptureFilter::compile(&config, "/repo").is_err());
    }

    #[test]
    fn a_rooted_pattern_does_not_reach_outside_the_project() {
        // `target/**` is anchored: it means this project's target directory,
        // not any directory called target anywhere on the machine. A pattern
        // meant to travel is written without a slash instead.
        let anchored = filter(&["target/**"], "/repo");
        let floating = filter(&["target"], "/repo");

        assert!(!anchored.excludes("/elsewhere/target/debug/app"));
        assert!(floating.excludes("/elsewhere/target"));
    }
}
