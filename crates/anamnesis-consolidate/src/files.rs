//! Pulling file paths out of observation bodies.
//!
//! This is a heuristic and is labelled as one wherever its output is shown. A
//! tool body is whatever the harness sent — sometimes JSON with a clean `path`
//! field, sometimes a shell command, sometimes prose. Rather than pretend to
//! parse every harness's shape, this looks for things that are unambiguously
//! file-like and accepts that it will miss some.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use anamnesis_core::observation::Observation;
use regex::Regex;

/// Extensions worth recognising. Restricting to a known list is what keeps
/// version numbers (`1.95`) and domains (`example.com`) out of the results.
const EXTENSIONS: &str =
    "rs|toml|md|json|ya?ml|sql|sh|ps1|ts|tsx|js|jsx|py|go|java|kt|rb|c|h|cpp|hpp|css|html";

/// File paths mentioned across a session's observations, deduplicated and
/// sorted for stable output.
pub fn mentioned_files(observations: &[Observation]) -> Vec<String> {
    let pattern = pattern();
    let mut found = BTreeSet::new();

    for observation in observations {
        for capture in pattern.find_iter(observation.body.as_str()) {
            let path = normalize(capture.as_str());
            if is_plausible(&path) {
                found.insert(path);
            }
        }
    }

    found.into_iter().collect()
}

/// The compiled path pattern.
fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)\b[\w.\-]+(?:[/\\][\w.\-]+)*\.(?:{EXTENSIONS})\b"
        ))
        .expect("file pattern is valid")
    })
}

/// Normalise separators and strip surrounding punctuation.
fn normalize(raw: &str) -> String {
    raw.replace('\\', "/")
        .trim_matches(|c: char| c == '"' || c == '\'' || c == ',')
        .to_owned()
}

/// Reject matches that are file-shaped but almost certainly not files.
fn is_plausible(path: &str) -> bool {
    if path.len() > 200 {
        return false;
    }
    // A bare `foo.rs` is a file; `1.95` or `v0.9` is a version that happens to
    // end in something list-like only if the extension list is loose.
    let stem = path.rsplit('/').next().unwrap_or(path);
    let Some((name, _)) = stem.rsplit_once('.') else {
        return false;
    };
    !name.is_empty() && !name.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::{ObservationId, SessionId};
    use anamnesis_core::observation::{BoundedBody, EventKind};
    use jiff::Timestamp;

    fn observation(body: &str) -> Observation {
        Observation {
            id: ObservationId::new(),
            session_id: SessionId::new(),
            kind: EventKind::ToolUse,
            tool: None,
            at: Timestamp::now(),
            body: BoundedBody::truncating(body, BoundedBody::DEFAULT_LIMIT),
            sanitized: true,
        }
    }

    #[test]
    fn finds_paths_in_json_and_in_prose() {
        let files = mentioned_files(&[
            observation(r#"{"file_path": "crates/core/src/lib.rs"}"#),
            observation("ran cargo test after editing Cargo.toml"),
        ]);
        assert!(files.contains(&"crates/core/src/lib.rs".to_owned()));
        assert!(files.contains(&"Cargo.toml".to_owned()));
    }

    #[test]
    fn normalises_windows_separators() {
        let files = mentioned_files(&[observation(r"edited crates\store\src\lib.rs")]);
        assert_eq!(files, vec!["crates/store/src/lib.rs".to_owned()]);
    }

    #[test]
    fn deduplicates_repeated_mentions() {
        let files =
            mentioned_files(&[observation("src/main.rs"), observation("src/main.rs again")]);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn ignores_version_numbers_and_domains() {
        let files = mentioned_files(&[observation(
            "rust 1.95 fetched from crates.io and example.com",
        )]);
        assert!(files.is_empty(), "matched: {files:?}");
    }

    #[test]
    fn output_order_is_stable() {
        let first = mentioned_files(&[observation("b.rs a.rs c.rs")]);
        let second = mentioned_files(&[observation("c.rs b.rs a.rs")]);
        assert_eq!(first, second);
    }

    #[test]
    fn unknown_extensions_are_left_alone() {
        let files = mentioned_files(&[observation("opened notes.xyz and image.png")]);
        assert!(files.is_empty(), "matched: {files:?}");
    }
}
