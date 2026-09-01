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

/// How a harness stores the servers it launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A JSON object under `mcpServers`.
    Json,
    /// TOML tables under `mcp_servers`.
    Toml,
}

/// Where one harness keeps its MCP registrations, and in what shape.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    /// The `--agent` name.
    pub agent: &'static str,
    /// Path components under the project root.
    pub config: &'static [&'static str],
    /// The shape of that file.
    pub format: Format,
    /// Anything a person needs to know that the file cannot say.
    pub note: &'static str,
}

/// Claude Code reads `.mcp.json` from the project root.
pub const CLAUDE_CODE: Target = Target {
    agent: "claude-code",
    config: &[".mcp.json"],
    format: Format::Json,
    note: "Read from the project root, and often committed - see the warning below.",
};

/// Cursor keeps the same shape in its own directory.
pub const CURSOR: Target = Target {
    agent: "cursor",
    config: &[".cursor", "mcp.json"],
    format: Format::Json,
    note: "`~/.cursor/mcp.json` is the same file for every project, if that is what you want.",
};

/// Gemini CLI puts MCP servers in the settings file its hooks already live in.
pub const GEMINI_CLI: Target = Target {
    agent: "gemini-cli",
    config: &[".gemini", "settings.json"],
    format: Format::Json,
    note: "The same file `install-hooks` writes to; only the `mcpServers` key is touched.",
};

/// Codex is the one that is not JSON.
pub const CODEX: Target = Target {
    agent: "codex",
    config: &[".codex", "config.toml"],
    format: Format::Toml,
    note: "TOML, and the table is `mcp_servers` rather than `mcpServers`.",
};

/// Every harness this can register with.
pub const TARGETS: [Target; 4] = [CLAUDE_CODE, CURSOR, GEMINI_CLI, CODEX];

/// The target for an agent name, if there is one.
pub fn target(agent: &str) -> Option<Target> {
    TARGETS
        .into_iter()
        .find(|target| target.agent.eq_ignore_ascii_case(agent))
}

/// Why a harness that speaks MCP still cannot be registered this way.
pub fn cannot_register(agent: &str) -> Option<&'static str> {
    match agent.trim().to_ascii_lowercase().as_str() {
        "opencode" => Some(
            "OpenCode extends through a TypeScript plugin API rather than a\n  \
             configuration file, which is the same reason `install-hooks` cannot\n  \
             wire it. The server itself is fine there: `anamnesis mcp --repo <dir>`\n  \
             over stdio is what any harness needs.",
        ),
        _ => None,
    }
}

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
/// Some of these files a project may well commit, and what goes in them is
/// specific to one machine — so the report says as much rather than leaving it
/// to be discovered by a colleague whose checkout points at somebody else's
/// home directory.
pub fn config_path(target: &Target, root: &Path) -> PathBuf {
    target
        .config
        .iter()
        .fold(root.to_path_buf(), |path, component| path.join(component))
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

/// Take the anamnesis server back out, and nothing else with it.
///
/// The inverse of [`register`]. Only the entry under `name` goes; every other
/// MCP server in the file is somebody's and stays. An `mcpServers` object left
/// empty is removed too, so the file does not read as configured when nothing
/// is configured.
///
/// Returns whether anything was there.
pub fn unregister(config: &mut Value, name: &str) -> bool {
    let Some(root) = config.as_object_mut() else {
        return false;
    };
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    let removed = servers.remove(name).is_some();
    if servers.is_empty() {
        root.remove("mcpServers");
    }
    removed
}

/// The same, for the harness that keeps its servers in TOML.
///
/// `toml_edit` rather than a re-serialise, for the reason registration uses
/// it: the comments and key order in somebody's `config.toml` are theirs, and
/// an uninstall that reformatted the file would be taking more than it was
/// asked for.
pub fn unregister_toml(document: &mut toml_edit::DocumentMut, name: &str) -> bool {
    let Some(servers) = document
        .get_mut("mcp_servers")
        .and_then(|item| item.as_table_mut())
    else {
        return false;
    };
    let removed = servers.remove(name).is_some();
    if servers.is_empty() {
        document.as_table_mut().remove("mcp_servers");
    }
    removed
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

/// Put the server into a TOML configuration, leaving everything else alone.
///
/// Codex is the one harness that is not JSON, and `toml_edit` is used rather
/// than a parse-and-reserialize for the reason it exists: this is somebody's
/// configuration file, with their comments and their key order in it, and a
/// round trip through a plain data model would hand it back rearranged and
/// stripped. The same rule as the JSON side, kept by a different means.
pub fn register_toml(
    document: &mut toml_edit::DocumentMut,
    name: &str,
    entry: &Value,
) -> Registration {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let servers = document["mcp_servers"].or_insert(toml_edit::table());
    // Implicit, so the file gets `[mcp_servers.anamnesis]` rather than an empty
    // `[mcp_servers]` header above it.
    if let Some(table) = servers.as_table_mut() {
        table.set_implicit(true);
    }

    let previous = servers.get(name).map(describe_toml);
    let mut table = toml_edit::Table::new();
    table["command"] = toml_edit::value(command);
    let mut list = toml_edit::Array::new();
    for arg in &args {
        list.push(*arg);
    }
    table["args"] = toml_edit::value(list);

    match previous {
        Some(previous) if previous == describe(entry) => Registration::Unchanged,
        Some(previous) => {
            servers[name] = toml_edit::Item::Table(table);
            Registration::Replaced(previous)
        }
        None => {
            servers[name] = toml_edit::Item::Table(table);
            Registration::Added
        }
    }
}

/// How a TOML entry reads in a report, in the same words as a JSON one.
fn describe_toml(item: &toml_edit::Item) -> String {
    let command = item
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or("<no command>");
    let args: Vec<&str> = item
        .get("args")
        .and_then(|value| value.as_array())
        .map(|args| args.iter().filter_map(|arg| arg.as_str()).collect())
        .unwrap_or_default();
    if args.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

/// Read a TOML configuration, or start an empty one.
///
/// A file that exists and does not parse is an error rather than something to
/// replace, exactly as on the JSON side: it is somebody's configuration and
/// possibly the only copy.
pub fn read_toml(path: &Path) -> anyhow::Result<toml_edit::DocumentMut> {
    if !path.exists() {
        return Ok(toml_edit::DocumentMut::new());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(text.trim_start_matches('\u{feff}').parse()?)
}

/// Write a TOML configuration without ever leaving a partial one in place.
pub fn write_toml(path: &Path, document: &toml_edit::DocumentMut) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("toml.anamnesis-tmp");
    std::fs::write(&temporary, document.to_string().as_bytes())?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// What to paste, in the shape this harness reads.
pub fn render(target: &Target, entry: &Value) -> anyhow::Result<String> {
    Ok(match target.format {
        Format::Json => serde_json::to_string_pretty(
            &serde_json::json!({ "mcpServers": { SERVER_NAME: entry.clone() } }),
        )?,
        Format::Toml => {
            let mut document = toml_edit::DocumentMut::new();
            register_toml(&mut document, SERVER_NAME, entry);
            document.to_string().trim_end().to_owned()
        }
    })
}

/// Merge the registration into whatever the harness already has, and save it.
///
/// One entry point for both shapes, so the command reads the same for a
/// harness that keeps JSON and one that keeps TOML — and so that the rules
/// they share (merge, never clobber, refuse a file that will not parse) are
/// stated once rather than twice with a chance to diverge.
pub fn apply(
    target: &Target,
    path: &Path,
    name: &str,
    entry: &Value,
) -> anyhow::Result<Registration> {
    match target.format {
        Format::Json => {
            let mut existing = crate::hooks::read_settings(path)?;
            let outcome = register(&mut existing, name, entry);
            if outcome.changed() {
                crate::hooks::write_settings(path, &existing)?;
            }
            Ok(outcome)
        }
        Format::Toml => {
            let mut document = read_toml(path)?;
            let outcome = register_toml(&mut document, name, entry);
            if outcome.changed() {
                write_toml(path, &document)?;
            }
            Ok(outcome)
        }
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

    /// Codex is the only harness here that is not JSON, and the merge has to
    /// leave the rest of somebody's `config.toml` exactly as it was —
    /// comments, key order and all. A parse-and-reserialize would hand it back
    /// tidied, which is not what anybody asked for.
    #[test]
    fn a_toml_registration_leaves_the_rest_of_the_file_alone() {
        let mut document: toml_edit::DocumentMut = r#"
# My settings.
model = "gpt-5"

[mcp_servers.postgres]
command = "pgmcp"
args = ["--dsn", "postgres:///app"]
"#
        .parse()
        .expect("parses");

        assert_eq!(
            register_toml(&mut document, SERVER_NAME, &entry()),
            Registration::Added
        );

        let text = document.to_string();
        assert!(text.contains("# My settings."), "{text}");
        assert!(text.contains("model = \"gpt-5\""), "{text}");
        assert!(text.contains("[mcp_servers.postgres]"), "{text}");
        assert!(text.contains("[mcp_servers.anamnesis]"), "{text}");
        assert!(text.contains("/opt/anamnesis"), "{text}");
        assert!(
            !text.contains("[mcp_servers]\n"),
            "the parent table should stay implicit: {text}"
        );
    }

    #[test]
    fn registering_the_same_toml_server_twice_changes_nothing() {
        let mut document = toml_edit::DocumentMut::new();
        register_toml(&mut document, SERVER_NAME, &entry());
        assert_eq!(
            register_toml(&mut document, SERVER_NAME, &entry()),
            Registration::Unchanged
        );
    }

    #[test]
    fn a_stale_toml_entry_is_replaced_and_named() {
        let mut document: toml_edit::DocumentMut =
            "[mcp_servers.anamnesis]\ncommand = \"/old/anamnesis\"\nargs = [\"mcp\"]\n"
                .parse()
                .expect("parses");

        assert_eq!(
            register_toml(&mut document, SERVER_NAME, &entry()),
            Registration::Replaced("/old/anamnesis mcp".to_owned())
        );
    }

    /// Every harness this claims to register with has to have somewhere to be
    /// registered, and no two of them may share a file by accident.
    #[test]
    fn every_target_has_its_own_place() {
        let mut seen = Vec::new();
        for entry in TARGETS {
            assert!(!entry.config.is_empty(), "{} has no path", entry.agent);
            let path = config_path(&entry, Path::new("."));
            assert!(!seen.contains(&path), "two harnesses share {path:?}");
            seen.push(path);
            assert!(
                target(entry.agent).is_some(),
                "{} is unreachable by name",
                entry.agent
            );
        }
    }

    #[test]
    fn an_entry_without_arguments_still_describes_itself() {
        assert_eq!(
            describe(&serde_json::json!({ "command": "anamnesis" })),
            "anamnesis"
        );
    }
}
