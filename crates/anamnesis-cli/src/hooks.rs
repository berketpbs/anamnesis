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

/// Why a harness anamnesis knows about still cannot be wired this way.
///
/// Said rather than left as "not yet", because for one of them it is not a
/// gap that will close: an answer someone can act on beats a promise.
pub fn cannot_wire(agent: &str) -> Option<&'static str> {
    match agent {
        "opencode" => Some(
            "OpenCode extends through a TypeScript plugin API, not a command hook:\n  \
             a plugin is a module under .opencode/plugins/ that subscribes to events\n  \
             like `session.created` and returns context through the SDK client.\n  \
             There is no command for `install-hooks` to register, and writing the\n  \
             plugin here would mean guessing at how it hands a handoff back.",
        ),
        _ => None,
    }
}

/// What a merge did, per event.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Events newly wired to this command.
    pub added: Vec<String>,
    /// Events already wired to this command, left untouched.
    pub present: Vec<String>,
}

impl Outcome {
    /// Whether anything about the file would change.
    pub fn changed(&self) -> bool {
        !self.added.is_empty()
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

/// The command a hook runs, for this binary and this server.
pub fn hook_command(binary: &str, agent: &str, server: &str) -> String {
    format!("{binary} hook --agent {agent} --server {server}")
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

        let (Some(existing), Some(matchers)) = (existing.as_array_mut(), matchers.as_array())
        else {
            continue;
        };
        existing.extend(matchers.iter().cloned());
        outcome.added.push(event.clone());
    }

    outcome
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

    /// Pointing the same install at a different server is a different command,
    /// so it is a hook that is not there yet rather than one already wired.
    #[test]
    fn a_different_server_is_not_mistaken_for_the_same_hook() {
        let mut settings = Value::Object(Map::new());
        merge(&mut settings, &config());

        let elsewhere = hook_config(
            &CLAUDE_CODE,
            "anamnesis hook --agent claude-code --server http://other:9000",
        );
        let outcome = merge(&mut settings, &elsewhere);
        assert_eq!(outcome.added.len(), CLAUDE_CODE.events.len());
    }

    #[test]
    fn the_command_names_the_binary_the_agent_and_the_server() {
        let command = hook_command("/usr/bin/anamnesis", "claude-code", "http://127.0.0.1:8080");
        assert_eq!(
            command,
            "/usr/bin/anamnesis hook --agent claude-code --server http://127.0.0.1:8080"
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

    /// A harness that extends through something other than a command hook has
    /// to say so. Left as "not yet", someone waits for a release that is never
    /// coming.
    #[test]
    fn opencode_is_told_why_rather_than_when() {
        let reason = cannot_wire("opencode").expect("a reason");
        assert!(reason.contains("plugin"), "{reason}");
        assert!(
            cannot_wire("codex").is_none(),
            "codex is wired, not refused"
        );
        assert!(cannot_wire("claude-code").is_none());
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
}
