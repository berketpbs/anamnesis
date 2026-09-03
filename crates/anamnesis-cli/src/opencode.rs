//! Wiring OpenCode, which extends through a plugin rather than a command.
//!
//! Every other harness `install-hooks` knows takes a command and a list of
//! event names in a settings file. OpenCode takes a module: a file under
//! `.opencode/plugins/` exporting an async function that returns an object of
//! hooks. The difference goes all the way down — there is no command string to
//! merge, nothing to compare against what is already there, and no stdout
//! channel for a handoff to arrive on.
//!
//! So this module writes a file instead of merging JSON, and the file it
//! writes is checked into this repository beside it rather than built out of
//! string fragments: a plugin is code, and code assembled by `format!` is code
//! nobody reads before it ships.
//!
//! **Two values are baked in.** The path to this binary and the address of the
//! server, both written through `serde_json` so that they arrive as string
//! literals whatever they contain. That is not a formality on Windows: a path
//! carries backslashes, every backslash inside a JavaScript string is an
//! escape, and `C:\Users\...` pasted raw becomes `C:Users...` — the same
//! character class that stopped this repository's own capture for two days
//! when a hook command reached a shell unquoted.
//!
//! **A file this command did not write is never overwritten.** The plugin
//! carries a marker line; without it the file belongs to somebody else and the
//! command says so rather than replacing their work.

use std::path::{Path, PathBuf};

/// The plugin, with `{{BINARY}}` and `{{SERVER}}` still in it.
const TEMPLATE: &str = include_str!("../assets/opencode-plugin.js");

/// Where the plugin goes, under the project root.
pub const PLUGIN_PATH: [&str; 3] = [".opencode", "plugins", "anamnesis.js"];

/// The line that says this file is ours to rewrite.
///
/// Deliberately narrow: the first line of what this command writes, not a
/// mention of anamnesis anywhere in the file. Somebody's own plugin that
/// happens to call `anamnesis hook` is theirs, and replacing it would throw
/// away whatever else they had it doing.
const MARKER: &str = "// anamnesis — long-term memory for AI coding agents.";

/// What writing the plugin did.
#[derive(Debug, PartialEq, Eq)]
pub enum Written {
    /// There was no plugin, and now there is.
    Created,
    /// A plugin this command wrote was replaced with the current one.
    Rewritten,
    /// It was already exactly this, down to the baked-in paths.
    Unchanged,
    /// A file is there and this command did not write it.
    Foreign,
}

/// The plugin source, with this machine's binary and server in it.
pub fn plugin(binary: &str, server: &str) -> String {
    // Forward slashes even on Windows: the path lands in a JavaScript string
    // literal, and every API that reads it — `Bun.spawn`, the Windows loader —
    // takes either separator. `serde_json` would escape the backslashes
    // correctly anyway; this is so the file reads as something a person could
    // have typed.
    let binary = binary.replace('\\', "/");
    TEMPLATE
        .replace("{{BINARY}}", &quoted(&binary))
        .replace("{{SERVER}}", &quoted(server))
}

/// Where the plugin belongs under `root`.
pub fn plugin_path(root: &Path) -> PathBuf {
    PLUGIN_PATH
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// Whether the plugin at `path` is one this command wrote.
///
/// The same marker [`write()`] checks, asked from the other side: uninstalling
/// may remove our file and must not remove somebody else's.
pub fn is_ours(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.starts_with(MARKER))
}

/// Write the plugin, unless the file there belongs to somebody else.
pub fn write(path: &Path, source: &str) -> std::io::Result<Written> {
    let existing = std::fs::read_to_string(path).ok();
    let outcome = match &existing {
        None => Written::Created,
        Some(text) if !text.starts_with(MARKER) => return Ok(Written::Foreign),
        Some(text) if text == source => return Ok(Written::Unchanged),
        Some(_) => Written::Rewritten,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, source)?;
    Ok(outcome)
}

/// A JavaScript string literal holding `value`.
///
/// JSON string syntax is a subset of JavaScript's, so this is exact rather
/// than approximately right: quotes, backslashes and control characters all
/// come out escaped.
fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this repository already paid for once, in a different file.
    /// A Windows path in a JavaScript string is a path full of escapes unless
    /// something turns it around.
    #[test]
    fn a_windows_path_survives_being_a_javascript_string() {
        let source = plugin(
            r"C:\Users\Ada Lovelace\AppData\Roaming\anamnesis\bin\anamnesis.exe",
            "http://127.0.0.1:8080",
        );

        assert!(
            source.contains(
                "const BINARY = \"C:/Users/Ada Lovelace/AppData/Roaming/anamnesis/bin/anamnesis.exe\";"
            ),
            "{}",
            first_lines(&source, 30)
        );
        assert!(
            !source.contains(r"C:\Users"),
            "a backslash reached the plugin: {}",
            first_lines(&source, 30)
        );
    }

    /// Both values are quoted by a JSON serialiser rather than by hand, so a
    /// server address with something awkward in it cannot end the string
    /// early and turn the rest of the file into syntax.
    #[test]
    fn an_awkward_value_cannot_break_out_of_its_string() {
        let source = plugin("anamnesis", "http://x/\" + evil() + \"");

        assert!(
            source.contains(r#"const SERVER = "http://x/\" + evil() + \"";"#),
            "{}",
            first_lines(&source, 30)
        );
    }

    #[test]
    fn the_plugin_carries_every_lifecycle_event_the_parser_knows() {
        let source = plugin("anamnesis", "http://127.0.0.1:8080");

        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PostToolUse",
            "PreCompact",
            "SessionEnd",
        ] {
            assert!(source.contains(event), "{event} is not sent by the plugin");
        }
    }

    /// The hooks the plugin subscribes to are OpenCode's, spelled its way.
    /// Pinned here because a typo in one of these keys is a hook that is never
    /// called, and nothing about that failure is visible from either side.
    #[test]
    fn the_plugin_subscribes_by_the_names_opencode_uses() {
        let source = plugin("anamnesis", "http://127.0.0.1:8080");

        for hook in [
            "\"chat.message\"",
            "\"tool.execute.after\"",
            "\"experimental.session.compacting\"",
            "\"experimental.chat.system.transform\"",
            "dispose:",
        ] {
            assert!(source.contains(hook), "{hook} is missing from the plugin");
        }
    }

    #[test]
    fn writing_creates_then_leaves_alone_then_rewrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = plugin_path(dir.path());
        let source = plugin("anamnesis", "http://127.0.0.1:8080");

        assert_eq!(write(&path, &source).expect("write"), Written::Created);
        assert!(path.exists());

        assert_eq!(write(&path, &source).expect("write"), Written::Unchanged);

        let moved = plugin("anamnesis", "http://127.0.0.1:9000");
        assert_eq!(write(&path, &moved).expect("write"), Written::Rewritten);
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("9000"),
            "the plugin was not rewritten"
        );
        assert!(
            !std::fs::read_to_string(&path)
                .expect("read")
                .contains("8080"),
            "the old address survived a rewrite"
        );
    }

    /// Somebody else's plugin at that path is theirs. Overwriting it would be
    /// this command deciding that a file it has never seen was disposable.
    #[test]
    fn a_plugin_this_command_did_not_write_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = plugin_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, "export const Mine = async () => ({});\n").expect("theirs");

        let outcome = write(&path, &plugin("anamnesis", "http://127.0.0.1:8080")).expect("write");

        assert_eq!(outcome, Written::Foreign);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "export const Mine = async () => ({});\n",
            "somebody else's plugin was overwritten"
        );
    }

    /// The payloads the plugin sends, exactly as it builds them, read by the
    /// parser that will receive them. This is the contract between a file
    /// written in JavaScript and a crate written in Rust, and nothing else in
    /// either language checks it.
    #[test]
    fn the_payloads_the_plugin_sends_are_the_payloads_the_parser_reads() {
        use anamnesis_core::observation::EventKind;
        use anamnesis_core::session::AgentKind;
        use serde_json::json;

        let agent: AgentKind = "opencode".parse().expect("agent");
        let cases = [
            (
                json!({
                    "session_id": "ses_abc",
                    "hook_event_name": "SessionStart",
                    "cwd": "/repo",
                    "source": "startup"
                }),
                EventKind::SessionStart,
            ),
            (
                json!({
                    "session_id": "ses_abc",
                    "hook_event_name": "UserPromptSubmit",
                    "cwd": "/repo",
                    "prompt": "why is capture stopping"
                }),
                EventKind::UserPrompt,
            ),
            (
                json!({
                    "session_id": "ses_abc",
                    "hook_event_name": "PostToolUse",
                    "cwd": "/repo",
                    "tool_name": "read",
                    "tool_input": {"filePath": "src/main.rs"},
                    "tool_response": {"title": "src/main.rs", "output": "fn main() {}"}
                }),
                EventKind::ToolUse,
            ),
            (
                json!({
                    "session_id": "ses_abc",
                    "hook_event_name": "PreCompact",
                    "cwd": "/repo",
                    "trigger": "compacting"
                }),
                EventKind::PreCompact,
            ),
            (
                json!({
                    "session_id": "ses_abc",
                    "hook_event_name": "SessionEnd",
                    "cwd": "/repo",
                    "reason": "opencode exited"
                }),
                EventKind::SessionEnd,
            ),
        ];

        for (payload, expected) in cases {
            let parsed = anamnesis_hooks::parse(&agent, &payload).expect("parse");
            assert_eq!(parsed.kind, expected, "{payload}");
            assert_eq!(parsed.agent_session_id, "ses_abc");
            assert_eq!(parsed.cwd.as_deref(), Some(Path::new("/repo")));
        }
    }

    /// The tool event carries the file it touched where the parser looks for
    /// it, which is what `[capture] ignore_paths` reads. A path the parser
    /// cannot see is a path a project cannot exclude.
    #[test]
    fn a_tool_event_names_the_file_it_touched() {
        use anamnesis_core::session::AgentKind;
        use serde_json::json;

        let agent: AgentKind = "opencode".parse().expect("agent");
        let parsed = anamnesis_hooks::parse(
            &agent,
            &json!({
                "session_id": "ses_abc",
                "hook_event_name": "PostToolUse",
                "cwd": "/repo",
                "tool_name": "read",
                "tool_input": {"filePath": ".env"},
                "tool_response": {"output": "SECRET=..."}
            }),
        )
        .expect("parse");

        assert_eq!(parsed.paths, [".env"]);
    }

    fn first_lines(text: &str, count: usize) -> String {
        text.lines().take(count).collect::<Vec<_>>().join("\n")
    }
}
