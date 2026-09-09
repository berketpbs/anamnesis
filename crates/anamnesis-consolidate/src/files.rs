//! Pulling file paths out of observation bodies.
//!
//! This is a heuristic and is labelled as one wherever its output is shown. A
//! tool body is whatever the harness sent — sometimes JSON with a clean `path`
//! field, sometimes a shell command, sometimes prose. Rather than pretend to
//! parse every harness's shape, this looks for things that are unambiguously
//! file-like and accepts that it will miss some.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use anamnesis_core::observation::{EventKind, Observation, RESULT_MARKER};
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

/// Tool names that change a file, by the last segment of the name.
///
/// Taken from what this project's own index actually holds — `Write` (201
/// calls) and `Edit` (176) — plus the names the other harnesses give the same
/// two moments. Matched on the last `__`-separated segment so that an MCP tool
/// registered as `mcp__something__write` is recognised as the write it is.
///
/// `Bash` is deliberately absent, and it is by far the most used tool here
/// (3,070 calls). A shell command can obviously change a file; deciding
/// *which* file from the text of a command means parsing every shell, and a
/// wrong answer here is worse than no answer — this list is what a page will
/// claim the session changed.
const WRITING_TOOLS: &[&str] = &[
    "write",
    "edit",
    "multiedit",
    "notebookedit",
    "create",
    "createfile",
    "create_file",
    "applypatch",
    "apply_patch",
    "str_replace_editor",
];

/// Whether a tool of this name changes the file it names.
fn is_writing_tool(name: &str) -> bool {
    let last = name
        .rsplit("__")
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    WRITING_TOOLS.contains(&last.as_str())
}

/// The files a session actually changed, as distinct from the ones it read.
///
/// A page that lists both together says a session was *about* every file it
/// happened to open, which on a session that read forty files and edited two
/// is forty-two claims of which two are true. The entities are drawn from this
/// list first for the same reason: what a session changed is what a later
/// search is looking for.
///
/// Only completed calls count. An attempt that never came back may or may not
/// have written anything, and a page that lists a file as changed when the
/// call failed is worse than one that leaves it out.
///
/// The file comes from the tool's own input field when the body parses as JSON
/// — which is what every harness seen so far sends — and from the path pattern
/// otherwise. An `Edit` body carries the replaced text as well, and that text
/// can name other files; taking the declared field first is what keeps those
/// out of a list that says "this changed".
pub fn changed_files(observations: &[Observation]) -> Vec<String> {
    let mut found = BTreeSet::new();

    for observation in observations {
        if observation.kind != EventKind::ToolUse {
            continue;
        }
        let Some(tool) = &observation.tool else {
            continue;
        };
        if !is_writing_tool(&tool.name) {
            continue;
        }

        // The result is not part of what was asked for, and a command's output
        // can name any number of files it did not touch.
        let input = observation
            .body
            .as_str()
            .split(RESULT_MARKER)
            .next()
            .unwrap_or_default();

        if let Some(declared) = declared_path(input) {
            found.insert(declared);
            continue;
        }
        if let Some(capture) = pattern().find(input) {
            let path = normalize(capture.as_str());
            if is_plausible(&path) {
                found.insert(path);
            }
        }
    }

    found.into_iter().collect()
}

/// The path a tool input names outright, when the body is the JSON a harness
/// sends and one of the known keys is in it.
fn declared_path(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    let object = value.as_object()?;
    for key in [
        "file_path",
        "filePath",
        "notebook_path",
        "notebookPath",
        "path",
    ] {
        if let Some(raw) = object.get(key).and_then(serde_json::Value::as_str) {
            let path = normalize(raw);
            if is_plausible(&path) {
                return Some(path);
            }
        }
    }
    None
}

/// The compiled path pattern.
///
/// Two things here are about Windows, and both were losing most of a path.
///
/// The drive prefix is optional and spelled out. Without it an absolute path
/// loses its head: `C:` is not a path character, so no match can start at the
/// drive letter and one starts at the component after it instead.
/// `C:\Berke\anamnesis\src\lib.rs` was recorded as `Berke/anamnesis/src/lib.rs`,
/// which names a directory that does not exist.
///
/// Separators repeat, because a tool body is usually JSON and a backslash in
/// JSON is written twice. Matching exactly one separator meant that the moment
/// a path arrived through the field it most often arrives through, every
/// component but the last fell off: the same path above came back as
/// `lib.rs`. That one is worse than the drive, because a bare filename still
/// looks like a plausible answer.
fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)\b(?:[A-Za-z]:[/\\]+)?[\w.\-]+(?:[/\\]+[\w.\-]+)*\.(?:{EXTENSIONS})\b"
        ))
        .expect("file pattern is valid")
    })
}

/// Normalise separators and strip surrounding punctuation.
///
/// Runs of separators collapse to one: the pattern accepts them because JSON
/// doubles a backslash, and `C://Berke//src//lib.rs` is not a path anybody
/// wants to read back.
fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '\\' | '/' if out.ends_with('/') => {}
            '\\' => out.push('/'),
            other => out.push(other),
        }
    }
    out.trim_matches(|c: char| c == '"' || c == '\'' || c == ',')
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
    use anamnesis_core::observation::{BoundedBody, EventKind, ToolRef};
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

    fn by(tool: &str, body: &str) -> Observation {
        Observation {
            tool: Some(ToolRef {
                name: tool.to_owned(),
                ok: None,
                call_id: None,
            }),
            ..observation(body)
        }
    }

    /// The distinction the page is built on: a session that read forty files
    /// and edited two changed two.
    #[test]
    fn a_file_read_is_not_a_file_changed() {
        let changed = changed_files(&[
            by("Read", r#"{"file_path": "crates/core/src/page.rs"}"#),
            by("Edit", r#"{"file_path": "crates/core/src/lib.rs"}"#),
            by("Bash", r#"{"command": "cat crates/store/src/ops.rs"}"#),
        ]);

        assert_eq!(changed, vec!["crates/core/src/lib.rs".to_owned()]);
    }

    /// An edit carries the text it replaced, and that text names files the
    /// edit did not touch. The declared field is the one that means "this".
    #[test]
    fn an_edits_replaced_text_does_not_become_a_file_it_changed() {
        let changed = changed_files(&[by(
            "Edit",
            r#"{"file_path": "src/lib.rs", "old_string": "mod files; // see crates/other/src/thing.rs", "new_string": "mod files;"}"#,
        )]);

        assert_eq!(changed, vec!["src/lib.rs".to_owned()]);
    }

    /// A write that never came back may have written nothing. A page that
    /// lists it as changed is asserting something nobody observed.
    #[test]
    fn a_call_that_never_completed_changed_nothing() {
        let attempted = Observation {
            kind: EventKind::ToolAttempt,
            ..by("Write", r#"{"file_path": "src/never.rs"}"#)
        };

        assert!(changed_files(&[attempted]).is_empty());
    }

    /// A harness that registers its editor through MCP names it
    /// `mcp__server__write`, and it is still a write.
    #[test]
    fn a_namespaced_write_is_still_a_write() {
        let changed = changed_files(&[by(
            "mcp__editor__write",
            r#"{"file_path": "docs/readme.md"}"#,
        )]);

        assert_eq!(changed, vec!["docs/readme.md".to_owned()]);
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

    /// A path with a drive on it is the ordinary shape on Windows, and it used
    /// to arrive with its head missing: the drive letter cannot start a match,
    /// so the match started one component in and named a directory that is not
    /// there. Sessions recorded on this machine are full of the evidence.
    #[test]
    fn an_absolute_windows_path_keeps_its_drive() {
        let files = mentioned_files(&[observation(
            r"edited C:\Berke\anamnesis\crates\anamnesis-cli\src\hooks.rs today",
        )]);

        assert_eq!(
            files,
            vec!["C:/Berke/anamnesis/crates/anamnesis-cli/src/hooks.rs".to_owned()]
        );
    }

    /// The shape a path actually arrives in. A tool body is JSON, and JSON
    /// writes a backslash twice — so this is the common case on Windows, not
    /// the exotic one. Matching a single separator left only `reap.rs`, which
    /// is worse than losing the drive: a bare filename still reads like an
    /// answer. Caught by running the thing and reading what it wrote.
    #[test]
    fn a_path_escaped_for_json_keeps_all_of_its_components() {
        let files = mentioned_files(&[observation(
            r#"{"file_path":"C:\\Berke\\anamnesis\\crates\\anamnesis-web\\src\\reap.rs"}"#,
        )]);

        assert_eq!(
            files,
            vec!["C:/Berke/anamnesis/crates/anamnesis-web/src/reap.rs".to_owned()]
        );
    }

    /// Doubling is not the only run: a path pasted from a shell that already
    /// escaped it once can arrive with more.
    #[test]
    fn a_run_of_separators_collapses_to_one() {
        let files = mentioned_files(&[observation(r"crates\\\\store///src\\lib.rs")]);

        assert_eq!(files, vec!["crates/store/src/lib.rs".to_owned()]);
    }

    /// The drive is a letter, not the letter `C`, and the shell writes it in
    /// either case.
    #[test]
    fn any_drive_letter_in_either_case_is_kept() {
        for raw in [r"D:\work\main.rs", r"d:/work/main.rs"] {
            let files = mentioned_files(&[observation(raw)]);
            assert_eq!(files.len(), 1, "{raw}: {files:?}");
            assert!(
                files[0].to_lowercase().starts_with("d:/work/"),
                "{raw}: {files:?}"
            );
        }
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
