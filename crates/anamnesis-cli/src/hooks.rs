//! Wiring a harness's lifecycle hooks to anamnesis.
//!
//! Printing the configuration and asking someone to paste it works, but it is
//! the step where a setup most often stops halfway: the JSON goes into the
//! wrong file, or replaces hooks that were already there, or is pasted twice.
//! Writing it is the same operation with those mistakes removed — provided the
//! write itself can never be the thing that breaks someone's settings, which is
//! what most of this module is about.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Lifecycle events anamnesis wants, in one harness's own spelling.
///
/// The five are the same five everywhere: the session opening, what the person
/// asked for, what the agent did about it, the moment the context is about to
/// be thrown away, and the session closing. Only the names differ.
///
/// `PreCompact` is among them because a compaction is the harness admitting
/// the session no longer fits in its own context — precisely the moment a
/// durable memory is worth having.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Harness {
    /// The name `--agent` takes, and the one the hook command passes back.
    pub agent: &'static str,
    /// Configuration file, as path components under the project root.
    pub settings: &'static [&'static str],
    /// The five events, in this harness's spelling.
    pub events: &'static [&'static str],
    /// Schema version the file must declare, when the harness requires one.
    ///
    /// Cursor refuses a `hooks.json` without it. Written only when the file
    /// does not already say something — a version someone pinned themselves is
    /// theirs, and overwriting it would be this command deciding which schema
    /// their other hooks are written against.
    pub schema_version: Option<u64>,
    /// What to tell someone about this file after writing it.
    pub note: &'static str,
}

/// Claude Code: hooks live beside the rest of its settings.
pub const CLAUDE_CODE: Harness = Harness {
    agent: "claude-code",
    settings: &[".claude", "settings.local.json"],
    events: &[
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "PreCompact",
        "SessionEnd",
    ],
    schema_version: None,
    note: "Hooks are read when a session starts.",
};

/// Codex CLI: a file of its own, and the same five names.
///
/// The payloads match Claude Code's field for field — `session_id`, `cwd`,
/// `hook_event_name`, `tool_name`, `tool_input` — so nothing downstream had to
/// learn a second shape. What a `SessionStart` hook prints on stdout becomes
/// developer context, which is how the handoff arrives, exactly as it does in
/// Claude Code.
pub const CODEX: Harness = Harness {
    agent: "codex",
    settings: &[".codex", "hooks.json"],
    events: &[
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "PreCompact",
        "SessionEnd",
    ],
    schema_version: None,
    note: "Hooks are on unless `[features] hooks = false` says otherwise.",
};

/// Gemini CLI: the same five moments under four different names.
///
/// `BeforeAgent` fires after the user submits a prompt, `AfterTool` after a
/// tool runs, `PreCompress` before the context is compacted. Only
/// `SessionStart` and `SessionEnd` are spelled the way the others spell them,
/// which is exactly why the events belong in a table rather than in the code
/// that writes the file.
///
/// The payload fields are the ones everything downstream already reads —
/// `session_id`, `cwd`, `hook_event_name`, `prompt`, `tool_name`, `tool_input`
/// — so only the event names had to be taught. What differs is the way back:
/// Gemini requires stdout to be a single JSON object, so the handoff travels
/// as `hookSpecificOutput.additionalContext` rather than as plain text. See
/// `handoff_reply` in `main.rs`.
pub const GEMINI_CLI: Harness = Harness {
    agent: "gemini-cli",
    settings: &[".gemini", "settings.json"],
    events: &[
        "SessionStart",
        "BeforeAgent",
        "AfterTool",
        "PreCompress",
        "SessionEnd",
    ],
    schema_version: None,
    note: "Stdout must be one JSON object; the hook prints one.",
};

/// Cursor: camelCase events, its own file, and a schema version.
///
/// The first harness whose *payload* differs rather than only its names. It
/// identifies a session by `conversation_id`, gives the working directory as
/// `workspace_roots` on every event but `postToolUse`, and takes injected
/// context back as a top-level `additional_context`. All three are handled
/// where they belong — the first two in the parser, the third in
/// `handoff_reply`.
pub const CURSOR: Harness = Harness {
    agent: "cursor",
    settings: &[".cursor", "hooks.json"],
    events: &[
        "sessionStart",
        "beforeSubmitPrompt",
        "postToolUse",
        "preCompact",
        "sessionEnd",
    ],
    schema_version: Some(1),
    note: "Cursor reads hooks.json at startup.",
};

/// Every harness `install-hooks` can wire.
pub const HARNESSES: [Harness; 4] = [CLAUDE_CODE, CODEX, GEMINI_CLI, CURSOR];

/// The harness `agent` names, if it is one anamnesis can wire.
pub fn harness(agent: &str) -> Option<Harness> {
    HARNESSES.into_iter().find(|h| h.agent == agent)
}

/// What a merge did, per event.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Events newly wired to this command.
    pub added: Vec<String>,
    /// Events already wired to this command, left untouched.
    pub present: Vec<String>,
    /// Events whose anamnesis command was stale, and was rewritten in place.
    ///
    /// Kept apart from `added` because the file already pointed here, and the
    /// person needs to be told their old spelling was replaced rather than
    /// joined: two commands invoking the same binary would record every
    /// observation twice, and nothing downstream could tell the copies apart.
    pub replaced: Vec<String>,
}

impl Outcome {
    /// Whether anything about the file would change.
    pub fn changed(&self) -> bool {
        !self.added.is_empty() || !self.replaced.is_empty()
    }
}

/// The hook configuration for one harness, as its settings file holds it.
pub fn hook_config(harness: &Harness, command: &str) -> Value {
    let hooks: Map<String, Value> = harness
        .events
        .iter()
        .map(|event| {
            (
                (*event).to_owned(),
                serde_json::json!([{
                    "hooks": [{ "type": "command", "command": command }]
                }]),
            )
        })
        .collect();
    let mut config = Map::new();
    if let Some(version) = harness.schema_version {
        config.insert("version".to_owned(), Value::from(version));
    }
    config.insert("hooks".to_owned(), Value::Object(hooks));
    Value::Object(config)
}

/// An executable path a shell will still recognise after parsing it.
///
/// A harness does not execute this command itself, it hands the string to a
/// shell, and which shell that is belongs to the harness rather than to us.
/// That makes a bare Windows path the one spelling that cannot be written:
/// under `sh` every backslash is an escape, so `C:\Users\...\anamnesis.exe`
/// arrives as `C:UsersAppData...` and the shell reports a command it cannot
/// find — on stderr, which no harness shows, from a hook whose failure is
/// deliberately not allowed to interrupt the session. Capture stops, and the
/// settings file goes on looking exactly right.
///
/// Forward slashes are accepted by the Windows API, by `cmd`, and by every
/// POSIX shell, and they survive quoting. The quotes are what carry a path
/// with a space in it, which `C:\Users\Ada Lovelace\...` makes ordinary.
pub fn shell_path(binary: &str) -> String {
    format!("\"{}\"", binary.replace('\\', "/"))
}

/// The command a hook runs, for this binary and this server.
pub fn hook_command(binary: &str, agent: &str, server: &str) -> String {
    format!(
        "{} hook --agent {agent} --server {server}",
        shell_path(binary)
    )
}

/// Whether a command already in a settings file is anamnesis calling itself.
///
/// Deliberately narrow: the executable's own file name, and the subcommand it
/// is being asked to run. A hook someone else wrote that merely mentions
/// anamnesis — a wrapper script, a line that starts the server first — is
/// theirs, and this is the predicate deciding what may be overwritten.
fn is_ours(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(binary) = words.next() else {
        return false;
    };
    if words.next() != Some("hook") {
        return false;
    }
    let binary = binary.trim_matches('"').replace('\\', "/");
    let name = binary.rsplit('/').next().unwrap_or(binary.as_str());
    name.eq_ignore_ascii_case("anamnesis") || name.eq_ignore_ascii_case("anamnesis.exe")
}

/// Merge `incoming`'s hooks into `settings`, leaving everything else alone.
///
/// Two properties matter more than the merge itself. It never removes a hook
/// someone else put there — a project with its own `PostToolUse` hook keeps it,
/// and ours is appended beside it. And it is idempotent on the command string,
/// so running the command twice wires nothing twice; without that, the natural
/// response to "did that work?" is to run it again, and the harness would then
/// fire two copies of every event with nothing looking wrong.
pub fn merge(settings: &mut Value, incoming: &Value) -> Outcome {
    let mut outcome = Outcome::default();

    let Some(incoming_root) = incoming.as_object() else {
        return outcome;
    };
    let Some(incoming) = incoming_root.get("hooks").and_then(Value::as_object) else {
        return outcome;
    };

    // A settings file we would have to overwrite to proceed is left alone, and
    // reported as nothing added. The caller prints the configuration instead.
    let Some(root) = settings.as_object_mut() else {
        return outcome;
    };
    // Anything the harness requires beside its hooks — Cursor's schema
    // version — is written only when the file is silent about it. A value
    // already there is someone's, and this command has no business deciding
    // which schema their other hooks were written against.
    for (key, value) in incoming_root {
        if key != "hooks" && !root.contains_key(key) {
            root.insert(key.clone(), value.clone());
        }
    }

    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return outcome;
    };

    for (event, matchers) in incoming {
        let wanted = commands_in(matchers);
        let existing = hooks
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));

        let already = commands_in(existing);
        if wanted.iter().any(|command| already.contains(command)) {
            outcome.present.push(event.clone());
            continue;
        }

        // An anamnesis command that is not the one we want is this same
        // install run against a different path, port, or — the case this
        // exists for — a spelling of the path the harness's shell could not
        // read. Appending beside it would leave the broken line in place and
        // add a second one, so it is rewritten where it stands.
        if let Some(wanted) = wanted.first()
            && already.iter().any(|command| is_ours(command))
            && rewrite_ours(existing, wanted)
        {
            outcome.replaced.push(event.clone());
            continue;
        }

        let (Some(existing), Some(matchers)) = (existing.as_array_mut(), matchers.as_array())
        else {
            continue;
        };
        existing.extend(matchers.iter().cloned());
        outcome.added.push(event.clone());
    }

    outcome
}

/// Point every anamnesis command in this matcher list at `wanted`.
///
/// In place, so that whatever else the matcher carries — a `matcher` pattern,
/// a timeout, a key some later version of the harness added — is still there
/// afterwards. Rebuilding the entry from our own template would quietly drop
/// all of it, and this runs on files we did not write.
fn rewrite_ours(matchers: &mut Value, wanted: &str) -> bool {
    let Some(matchers) = matchers.as_array_mut() else {
        return false;
    };
    let mut rewrote = false;
    for hook in matchers
        .iter_mut()
        .filter_map(|matcher| matcher.get_mut("hooks")?.as_array_mut())
        .flatten()
    {
        let ours = hook
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(is_ours);
        if ours && let Some(command) = hook.get_mut("command") {
            *command = Value::from(wanted);
            rewrote = true;
        }
    }
    rewrote
}

/// Take anamnesis back out of a settings file, and nothing else with it.
///
/// The inverse of [`merge`], and it has the same obligation from the other
/// direction: a project with its own `PostToolUse` hook beside ours keeps it.
/// Only hook entries whose command is [`is_ours`] are removed — a wrapper
/// script somebody wrote that happens to call anamnesis is theirs, and
/// uninstalling must not take it.
///
/// Matchers left holding no hooks are removed, events left holding no matchers
/// are removed, and a `hooks` object left empty is removed: a settings file
/// that reads as configured when nothing is configured is the state this
/// codebase keeps finding at the bottom of a silent failure.
///
/// Returns the events it took something out of.
pub fn remove_ours(settings: &mut Value) -> Vec<String> {
    let mut cleaned = Vec::new();

    let Some(root) = settings.as_object_mut() else {
        return cleaned;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return cleaned;
    };

    for (event, matchers) in hooks.iter_mut() {
        let Some(matchers) = matchers.as_array_mut() else {
            continue;
        };
        let mut removed = false;

        for matcher in matchers.iter_mut() {
            let Some(entries) = matcher.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = entries.len();
            entries.retain(|hook| {
                !hook
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(is_ours)
            });
            removed |= entries.len() != before;
        }

        // A matcher that held only our hook is now an empty shell. Left
        // behind it would be a `PostToolUse` entry that matches everything
        // and runs nothing.
        matchers.retain(|matcher| {
            matcher
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|entries| !entries.is_empty())
        });

        if removed {
            cleaned.push(event.clone());
        }
    }

    hooks.retain(|_, matchers| {
        matchers
            .as_array()
            .is_none_or(|matchers| !matchers.is_empty())
    });
    let empty = hooks.is_empty();
    if empty {
        root.remove("hooks");
    }

    cleaned.sort();
    cleaned
}

/// Every `command` string reachable inside a settings file's matcher list.
fn commands_in(matchers: &Value) -> Vec<String> {
    matchers
        .as_array()
        .map(|matchers| {
            matchers
                .iter()
                .filter_map(|matcher| matcher.get("hooks")?.as_array())
                .flatten()
                .filter_map(|hook| hook.get("command")?.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Where a harness's project-local configuration lives, under `root`.
///
/// The project's rather than the user's, and for Claude Code the local file
/// rather than the shared one: `install-hooks` is run from inside a project and
/// points at that project's server, so writing hooks that fire in every
/// repository someone opens would be doing something they did not ask for.
/// `--settings` is there for anyone who wants exactly that.
pub fn default_settings_path(harness: &Harness, root: &Path) -> PathBuf {
    harness
        .settings
        .iter()
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

/// Read a settings file, or start an empty one.
///
/// A file that exists but does not parse is an error rather than something to
/// replace. It is someone's editor configuration, it may be the only copy of
/// it, and the caller's fallback — printing the JSON to paste by hand — costs
/// them a minute rather than their settings.
pub fn read_settings(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path)?;
    // The same byte-order-mark tolerance the hook path needs, for the same
    // reason: a settings file written by a PowerShell redirect starts with one.
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    Ok(serde_json::from_str(text)?)
}

/// Write a settings file without ever leaving a partial one in place.
///
/// This is someone's editor configuration and quite possibly the only copy.
/// `serde_json` is built with `preserve_order`, so what goes back keeps the key
/// order it was read in and the diff is only ever the hooks.
pub fn write_settings(path: &Path, settings: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = format!("{}\n", serde_json::to_string_pretty(settings)?);

    // Renamed into place from the same directory, because rename is only
    // atomic within one filesystem.
    let temporary = path.with_extension("json.anamnesis-tmp");
    std::fs::write(&temporary, body.as_bytes())?;
    // Windows refuses to rename onto an existing file, so clear the way first.
    // If the rename then fails, the temporary still holds the content.
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Value {
        hook_config(
            &CLAUDE_CODE,
            "anamnesis hook --agent claude-code --server http://localhost:8080",
        )
    }

    #[test]
    fn merging_into_an_empty_file_wires_every_event() {
        let mut settings = Value::Object(Map::new());
        let outcome = merge(&mut settings, &config());
        assert_eq!(outcome.added.len(), CLAUDE_CODE.events.len());
        assert!(outcome.present.is_empty());
    }

    /// Running the command twice is the natural response to "did that work?".
    /// If the second run wired everything again, the harness would fire two
    /// copies of every event and nothing would look wrong.
    #[test]
    fn merging_twice_wires_nothing_twice() {
        let mut settings = Value::Object(Map::new());
        merge(&mut settings, &config());
        let second = merge(&mut settings, &config());

        assert!(second.added.is_empty());
        assert_eq!(second.present.len(), CLAUDE_CODE.events.len());
        assert!(!second.changed());

        let matchers = settings["hooks"]["PostToolUse"].as_array().expect("array");
        assert_eq!(matchers.len(), 1);
    }

    /// The file belongs to the person whose editor reads it, and they may well
    /// have hooks of their own on the same events.
    #[test]
    fn a_hook_someone_else_wrote_survives() {
        let mut settings = serde_json::json!({
            "permissions": { "allow": ["Bash(git *)"] },
            "hooks": {
                "PostToolUse": [{
                    "hooks": [{ "type": "command", "command": "my-own-linter" }]
                }]
            }
        });

        merge(&mut settings, &config());

        let commands = commands_in(&settings["hooks"]["PostToolUse"]);
        assert!(
            commands.iter().any(|c| c == "my-own-linter"),
            "{commands:?}"
        );
        assert_eq!(commands.len(), 2);
        assert_eq!(settings["permissions"]["allow"][0], "Bash(git *)");
    }

    /// Pointing the same install at a different server moves the hooks rather
    /// than adding a second set. Adding was the old behaviour and it is not
    /// defensible: it leaves every event delivered twice, once to a server the
    /// person has just said they are no longer using, and the settings file
    /// gives no sign which of the two the memory came from.
    #[test]
    fn a_different_server_moves_the_hooks_rather_than_doubling_them() {
        let mut settings = Value::Object(Map::new());
        merge(&mut settings, &config());

        let elsewhere = hook_config(
            &CLAUDE_CODE,
            "anamnesis hook --agent claude-code --server http://other:9000",
        );
        let outcome = merge(&mut settings, &elsewhere);
        assert_eq!(outcome.replaced.len(), CLAUDE_CODE.events.len());
        assert!(outcome.added.is_empty());
        for event in CLAUDE_CODE.events {
            assert_eq!(commands_in(&settings["hooks"][event]).len(), 1);
        }
    }

    #[test]
    fn the_command_names_the_binary_the_agent_and_the_server() {
        let command = hook_command("/usr/bin/anamnesis", "claude-code", "http://127.0.0.1:8080");
        assert_eq!(
            command,
            "\"/usr/bin/anamnesis\" hook --agent claude-code --server http://127.0.0.1:8080"
        );
    }

    #[test]
    fn a_missing_settings_file_reads_as_an_empty_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = default_settings_path(&CLAUDE_CODE, dir.path());
        assert_eq!(
            read_settings(&path).expect("read"),
            Value::Object(Map::new())
        );
    }

    /// Someone's editor configuration is not ours to replace because we could
    /// not read it.
    #[test]
    fn a_settings_file_that_does_not_parse_is_an_error_not_a_fresh_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.local.json");
        std::fs::write(&path, "{ this is not json").expect("write");
        assert!(read_settings(&path).is_err());
    }

    #[test]
    fn a_settings_file_written_by_powershell_still_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.local.json");
        std::fs::write(&path, "\u{feff}{\"permissions\":{}}").expect("write");
        assert!(
            read_settings(&path)
                .expect("read")
                .get("permissions")
                .is_some()
        );
    }

    #[test]
    fn writing_creates_the_directory_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = default_settings_path(&CLAUDE_CODE, dir.path());

        let mut settings = Value::Object(Map::new());
        merge(&mut settings, &config());
        write_settings(&path, &settings).expect("write");

        assert_eq!(read_settings(&path).expect("read"), settings);
    }

    /// A temporary left behind would sit beside the settings file forever, and
    /// on Windows the second write would be renaming onto a name still in use.
    #[test]
    fn writing_twice_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.local.json");

        let settings = serde_json::json!({ "a": 1 });
        write_settings(&path, &settings).expect("first");
        write_settings(&path, &settings).expect("second");

        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("anamnesis-tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    /// The five events are the same five for every harness, whatever each one
    /// calls them. A harness wired for four of them captures a session with a
    /// hole in it, and nothing would say which.
    #[test]
    fn every_harness_wires_the_same_five_moments() {
        for harness in HARNESSES {
            assert_eq!(
                harness.events.len(),
                5,
                "{} wires {} events",
                harness.agent,
                harness.events.len()
            );
        }
    }

    /// Two harnesses writing to one path would have each overwrite the other's
    /// hooks, and the second `install-hooks` would look like it had worked.
    #[test]
    fn no_two_harnesses_write_to_the_same_file() {
        let root = Path::new("/project");
        let mut seen: Vec<PathBuf> = Vec::new();
        for harness in HARNESSES {
            let path = default_settings_path(&harness, root);
            assert!(
                !seen.contains(&path),
                "{} collides on {path:?}",
                harness.agent
            );
            seen.push(path);
        }
    }

    #[test]
    fn a_harness_is_found_by_the_name_the_flag_takes() {
        assert_eq!(harness("codex").expect("codex").agent, "codex");
        assert_eq!(
            harness("claude-code").expect("claude-code").agent,
            "claude-code"
        );
        assert!(harness("nonesuch").is_none());
    }

    /// Codex reads its own file rather than a settings file it shares with
    /// anything else, so the path is where the difference shows.
    #[test]
    fn codex_hooks_go_to_its_own_file() {
        let path = default_settings_path(&CODEX, Path::new("/project"));
        assert!(path.ends_with("hooks.json"), "{path:?}");
        assert!(path.to_string_lossy().contains(".codex"), "{path:?}");
    }

    /// The merge is shared, so a second harness has to inherit the property
    /// that matters most: running it twice wires nothing twice.
    #[test]
    fn wiring_codex_twice_changes_nothing_the_second_time() {
        let config = hook_config(&CODEX, "anamnesis hook --agent codex");
        let mut settings = Value::Object(Map::new());

        let first = merge(&mut settings, &config);
        assert_eq!(first.added.len(), CODEX.events.len());

        let second = merge(&mut settings, &config);
        assert!(!second.changed());
        assert_eq!(second.present.len(), CODEX.events.len());
    }

    /// OpenCode is not in this table, and that is not an omission: it extends
    /// through a plugin module rather than a command in a settings file, so
    /// `install-hooks` writes one (see `crate::opencode`) instead of merging
    /// JSON. Pinned here so that adding it to `HARNESSES` — which would write
    /// a command OpenCode never runs — fails a test rather than shipping.
    #[test]
    fn opencode_is_not_wired_through_a_settings_file() {
        assert!(harness("opencode").is_none());
        assert!(harness("codex").is_some());
        assert!(harness("claude-code").is_some());
    }

    /// Cursor refuses a `hooks.json` that does not declare its schema version,
    /// so the file this command writes has to carry one.
    #[test]
    fn cursor_gets_the_schema_version_its_file_requires() {
        let config = hook_config(&CURSOR, "anamnesis hook --agent cursor");
        assert_eq!(config["version"], 1);

        let mut settings = Value::Object(Map::new());
        merge(&mut settings, &config);
        assert_eq!(settings["version"], 1);
    }

    /// A version already in the file is somebody's, and this command has no
    /// business deciding which schema their other hooks were written against.
    #[test]
    fn a_version_already_there_is_left_alone() {
        let mut settings = serde_json::json!({ "version": 2, "hooks": {} });
        merge(
            &mut settings,
            &hook_config(&CURSOR, "anamnesis hook --agent cursor"),
        );
        assert_eq!(settings["version"], 2);
    }

    /// The harnesses that ask for no version must not acquire one.
    #[test]
    fn nothing_else_gains_a_version_key() {
        for harness in HARNESSES.iter().filter(|h| h.schema_version.is_none()) {
            let config = hook_config(harness, "anamnesis hook");
            assert!(
                config.get("version").is_none(),
                "{} grew a version key",
                harness.agent
            );
        }
    }

    /// The failure this whole path exists to avoid: a harness hands the command
    /// to a shell, and under `sh` a backslash is an escape. A Windows path
    /// written as it comes out of `current_exe` arrives as one unbroken word
    /// that names no file, the hook exits without reaching the server, and the
    /// settings file goes on looking exactly right.
    #[test]
    fn a_windows_path_survives_a_posix_shell() {
        let command = hook_command(
            r"C:\Users\Ada\AppData\Roaming\anamnesis\bin\anamnesis.exe",
            "claude-code",
            "http://127.0.0.1:8080",
        );
        assert!(!command.contains('\\'), "backslash left in {command}");
        assert!(command.starts_with('"'), "path not quoted in {command}");
        assert!(command.contains("/anamnesis/bin/anamnesis.exe\" hook "));
    }

    /// A path with a space in it is ordinary — `C:\Users\Ada Lovelace\…` — and
    /// only the quotes keep it one argument.
    #[test]
    fn a_path_with_a_space_stays_one_word() {
        let command = hook_command(r"C:\Users\Ada Lovelace\anamnesis.exe", "codex", "http://s");
        assert!(command.starts_with("\"C:/Users/Ada Lovelace/anamnesis.exe\" hook "));
    }

    /// The upgrade case, and the reason `replaced` exists. A file wired by an
    /// older install carries the unquoted spelling; wiring the new one beside
    /// it would leave the broken command in place and record everything twice
    /// once the harness's shell changed back.
    #[test]
    fn a_stale_anamnesis_command_is_rewritten_not_joined() {
        let stale = r"C:\bin\anamnesis.exe hook --agent claude-code --server http://127.0.0.1:8080";
        let mut settings = serde_json::json!({
            "hooks": { "SessionStart": [{ "hooks": [{ "type": "command", "command": stale }] }] }
        });

        let wanted = hook_command(
            r"C:\bin\anamnesis.exe",
            "claude-code",
            "http://127.0.0.1:8080",
        );
        let outcome = merge(&mut settings, &hook_config(&CLAUDE_CODE, &wanted));

        assert_eq!(outcome.replaced, vec!["SessionStart".to_owned()]);
        assert!(outcome.changed());
        let commands = commands_in(&settings["hooks"]["SessionStart"]);
        assert_eq!(commands, vec![wanted]);
    }

    /// The other half of that: a hook someone else wrote is not ours to
    /// rewrite, however much it mentions anamnesis.
    #[test]
    fn somebody_elses_hook_is_never_rewritten() {
        let theirs = "./scripts/start-anamnesis.sh && anamnesis serve";
        let mut settings = serde_json::json!({
            "hooks": { "SessionStart": [{ "hooks": [{ "type": "command", "command": theirs }] }] }
        });

        let wanted = hook_command("anamnesis", "claude-code", "http://127.0.0.1:8080");
        let outcome = merge(&mut settings, &hook_config(&CLAUDE_CODE, &wanted));

        assert_eq!(outcome.replaced, Vec::<String>::new());
        assert!(outcome.added.contains(&"SessionStart".to_owned()));
        let commands = commands_in(&settings["hooks"]["SessionStart"]);
        assert_eq!(commands, vec![theirs.to_owned(), wanted]);
    }

    /// Rewriting must not cost the matcher whatever else it carried.
    #[test]
    fn rewriting_keeps_the_rest_of_the_matcher() {
        let stale = "anamnesis hook --agent claude-code --server http://old:1";
        let mut settings = serde_json::json!({
            "hooks": { "PostToolUse": [{
                "matcher": "Edit|Write",
                "hooks": [{ "type": "command", "command": stale, "timeout": 5 }]
            }] }
        });

        let wanted = hook_command("anamnesis", "claude-code", "http://new:2");
        merge(&mut settings, &hook_config(&CLAUDE_CODE, &wanted));

        let matcher = &settings["hooks"]["PostToolUse"][0];
        assert_eq!(matcher["matcher"], "Edit|Write");
        assert_eq!(matcher["hooks"][0]["timeout"], 5);
        assert_eq!(matcher["hooks"][0]["command"], Value::from(wanted));
    }
}
