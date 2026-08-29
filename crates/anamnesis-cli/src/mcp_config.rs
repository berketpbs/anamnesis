//! Registering the MCP server with a harness.
//!
//! Hooks and MCP are the two halves of connecting an agent, and only one of
//! them had a command. `install-hooks` exists because setup steps that are not
//! written down fail silently; the MCP half was a line in the documentation,
//! and on the machine this project is developed on it went four months without
//! being run. Nothing said so: hooks captured every session, the wiki filled
//! up, and the agent could not read a word of it except the handoff it was
//! handed at startup.
//!
//! The documented line was `claude mcp add anamnesis -- anamnesis mcp`, which
//! assumes the binary is on `PATH`. It is not, here — it is copied to
//! `%APPDATA%\anamnesis\bin\` precisely so that `cargo build` can overwrite the
//! one in `target/` — so following the documentation literally would have
//! registered a server that cannot start, which is its own quiet failure.
//! Everything written here names the binary that is actually running.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// What the server is called in a harness's configuration.
pub const SERVER_NAME: &str = "anamnesis";

/// What happened to one registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The server was not there and has been added.
    Added,
    /// It was already registered, identically, and nothing was touched.
    Unchanged,
    /// It was registered differently. The old command is carried back so the
    /// report can say what it replaced.
    Replaced(String),
}

impl Registration {
    /// Whether the file would change.
    pub fn changed(&self) -> bool {
        !matches!(self, Registration::Unchanged)
    }
}

/// Where a harness keeps its MCP servers, relative to a project.
///
/// Claude Code reads `.mcp.json` from the project root. Unlike the hook
/// settings, which live in an untracked `.claude/`, this file is one a project
/// may well commit — and what goes in it is specific to one machine, so the
/// report says as much rather than leaving it to be discovered by a colleague
/// whose checkout points at somebody else's home directory.
pub fn config_path(root: &Path) -> PathBuf {
    root.join(".mcp.json")
}

/// The entry that launches this binary as a stdio MCP server.
///
/// `--repo` is passed explicitly rather than left to default to `.`: the
/// harness chooses the working directory of the subprocess it spawns, and a
/// scope resolved from the wrong directory is the one failure this crate can
/// neither detect nor repair — it would answer, fluently, out of another
/// project's memory.
pub fn server_entry(binary: &Path, repo: &Path) -> Value {
    serde_json::json!({
        "command": binary.display().to_string(),
        "args": ["mcp", "--repo", repo.display().to_string()],
    })
}

/// Put `entry` into `config` under `name`, leaving every other server alone.
///
/// Idempotent on the whole entry, so running it twice to check the first time
/// worked — which is what people do — changes nothing and says so.
///
/// An entry under our own name that differs is replaced rather than refused.
/// This is where MCP and hooks part company: a hook list can hold a stranger's
/// hook beside ours, but a server map holds one entry per name, and an
/// `anamnesis` entry is ours by construction — almost always a path from
/// before the binary moved. Leaving a registration that cannot start would be
/// worse than replacing it, and the report names what it replaced so the change
/// is never silent.
pub fn register(config: &mut Value, name: &str, entry: &Value) -> Registration {
    if !config.is_object() {
        *config = Value::Object(Map::new());
    }
    let root = config.as_object_mut().expect("object");
    let servers = root
        .entry("mcpServers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    let servers = servers.as_object_mut().expect("object");

    match servers.get(name) {
        Some(existing) if existing == entry => Registration::Unchanged,
        Some(existing) => {
            let previous = describe(existing);
            servers.insert(name.to_owned(), entry.clone());
            Registration::Replaced(previous)
        }
        None => {
            servers.insert(name.to_owned(), entry.clone());
            Registration::Added
        }
    }
}

/// How an entry reads in a report: the command and its arguments, as run.
pub fn describe(entry: &Value) -> String {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("<no command>");
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if args.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {args}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Value {
        server_entry(Path::new("/opt/anamnesis"), Path::new("/work/project"))
    }

    #[test]
    fn an_entry_names_the_binary_and_the_repository_it_answers_for() {
        let entry = entry();
        assert_eq!(entry["command"], "/opt/anamnesis");
        assert_eq!(entry["args"][0], "mcp");
        assert_eq!(entry["args"][1], "--repo");
        assert_eq!(entry["args"][2], "/work/project");
    }

    #[test]
    fn registering_into_nothing_creates_the_map() {
        let mut config = Value::Object(Map::new());
        assert_eq!(
            register(&mut config, SERVER_NAME, &entry()),
            Registration::Added
        );
        assert_eq!(config["mcpServers"]["anamnesis"], entry());
    }

    /// Running it twice is what people do to check the first time worked.
    #[test]
    fn registering_the_same_server_twice_changes_nothing() {
        let mut config = Value::Object(Map::new());
        register(&mut config, SERVER_NAME, &entry());
        let second = register(&mut config, SERVER_NAME, &entry());

        assert_eq!(second, Registration::Unchanged);
        assert!(!second.changed());
        assert_eq!(
            config["mcpServers"].as_object().expect("map").len(),
            1,
            "a second run must not leave two copies"
        );
    }

    /// The case this is for: the binary moved, and the old entry names a path
    /// that no longer starts. Replacing it is the point; saying so is the rule.
    #[test]
    fn a_stale_entry_is_replaced_and_named() {
        let mut config = serde_json::json!({
            "mcpServers": {
                "anamnesis": { "command": "/old/anamnesis", "args": ["mcp"] }
            }
        });

        let outcome = register(&mut config, SERVER_NAME, &entry());

        assert_eq!(
            outcome,
            Registration::Replaced("/old/anamnesis mcp".to_owned())
        );
        assert_eq!(config["mcpServers"]["anamnesis"], entry());
    }

    /// Somebody else's server is not ours to touch, whatever else changes.
    #[test]
    fn another_server_is_left_exactly_as_it_was() {
        let mut config = serde_json::json!({
            "mcpServers": {
                "postgres": { "command": "pgmcp", "args": ["--dsn", "postgres:///app"] }
            },
            "somethingElse": { "kept": true }
        });

        register(&mut config, SERVER_NAME, &entry());

        assert_eq!(config["mcpServers"]["postgres"]["command"], "pgmcp");
        assert_eq!(
            config["mcpServers"]["postgres"]["args"][1],
            "postgres:///app"
        );
        assert_eq!(config["somethingElse"]["kept"], true);
        assert_eq!(config["mcpServers"]["anamnesis"], entry());
    }

    /// A file whose `mcpServers` is not a map cannot be merged into, and
    /// guessing what was meant would throw away whatever is there. It is
    /// replaced only because there is nothing in it to keep — a caller reading
    /// an unparseable file never gets this far.
    #[test]
    fn a_config_that_is_not_an_object_is_started_over() {
        let mut config = Value::String("nonsense".to_owned());
        assert_eq!(
            register(&mut config, SERVER_NAME, &entry()),
            Registration::Added
        );
        assert_eq!(config["mcpServers"]["anamnesis"], entry());
    }

    #[test]
    fn an_entry_without_arguments_still_describes_itself() {
        assert_eq!(
            describe(&serde_json::json!({ "command": "anamnesis" })),
            "anamnesis"
        );
    }
}
