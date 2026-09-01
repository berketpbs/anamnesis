//! `anamnesis uninstall`: take anamnesis back out of a machine's harnesses.
//!
//! The counterpart to `install-hooks` and `install-mcp`, and it has one
//! obligation those two also have from the other side: it may remove only what
//! anamnesis wrote. A settings file is somebody's editor configuration, it is
//! often the only copy of it, and a project with its own `PostToolUse` hook
//! beside ours keeps it. What identifies ours is the same narrow predicate the
//! installer uses — the executable's own name and the subcommand it runs — so
//! a wrapper script somebody wrote that happens to call anamnesis is theirs
//! and stays.
//!
//! **Memory is not touched.** Uninstalling stops the recording; it does not
//! remove what was recorded, and this command says where that is rather than
//! deciding for anybody. `anamnesis purge` removes one project's memory and
//! deleting the data directory removes all of it, and both are things a person
//! should type on purpose.

use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;

use crate::{hooks, mcp_config, opencode};

/// What was found in one file, and what would be left.
#[derive(Debug, PartialEq, Eq)]
pub struct Removal {
    /// The file it was found in.
    pub path: PathBuf,
    /// What is being taken out of it, in a person's words.
    pub what: String,
}

/// Take anamnesis out of every harness configuration under `root`.
pub fn cmd_uninstall(apply: bool, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let data = DataDir::resolve(data_dir).ok();

    let mut found: Vec<Removal> = Vec::new();
    let mut changes: Vec<Change> = Vec::new();

    // Hooks, one harness at a time.
    for harness in hooks::HARNESSES {
        let path = hooks::default_settings_path(&harness, &root);
        let Ok(mut settings) = hooks::read_settings(&path) else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        let cleaned = hooks::remove_ours(&mut settings);
        if !cleaned.is_empty() {
            found.push(Removal {
                path: path.clone(),
                what: format!("hooks: {}", cleaned.join(", ")),
            });
            changes.push(Change::Json { path, settings });
        }
    }

    // OpenCode's plugin is a file rather than a fragment of one, so what is
    // left behind is the file's absence rather than a smaller file.
    let plugin = opencode::plugin_path(&root);
    if opencode::is_ours(&plugin) {
        found.push(Removal {
            path: plugin.clone(),
            what: "the OpenCode plugin".to_owned(),
        });
        changes.push(Change::Delete { path: plugin });
    }

    // MCP registrations.
    for target in mcp_config::TARGETS {
        let path = mcp_config::config_path(&target, &root);
        if !path.exists() {
            continue;
        }
        match target.format {
            mcp_config::Format::Json => {
                let Ok(mut config) = hooks::read_settings(&path) else {
                    continue;
                };
                if mcp_config::unregister(&mut config, mcp_config::SERVER_NAME) {
                    found.push(Removal {
                        path: path.clone(),
                        what: format!("mcp server `{}`", mcp_config::SERVER_NAME),
                    });
                    changes.push(Change::Json {
                        path,
                        settings: config,
                    });
                }
            }
            mcp_config::Format::Toml => {
                let Ok(mut document) = mcp_config::read_toml(&path) else {
                    continue;
                };
                if mcp_config::unregister_toml(&mut document, mcp_config::SERVER_NAME) {
                    found.push(Removal {
                        path: path.clone(),
                        what: format!("mcp server `{}`", mcp_config::SERVER_NAME),
                    });
                    changes.push(Change::Toml { path, document });
                }
            }
        }
    }

    println!("🧹 Uninstalling anamnesis from {}", root.display());
    println!();

    if found.is_empty() {
        println!("  Nothing to remove: no harness here is wired to anamnesis.");
        report_memory(data.as_ref());
        return Ok(());
    }

    for removal in &found {
        println!("  {}", removal.path.display());
        println!("      {}", removal.what);
    }

    if !apply {
        println!();
        println!("  Nothing has been changed. Run again with --apply to carry this out.");
        println!();
        println!("  Only what anamnesis wrote is removed. Any other hook or MCP server");
        println!("  in those files stays exactly where it is.");
        report_memory(data.as_ref());
        return Ok(());
    }

    for change in changes {
        change.write()?;
    }

    println!();
    println!("  Removed. Harnesses read their configuration at startup, so this");
    println!("  takes effect in the next session.");
    report_memory(data.as_ref());
    Ok(())
}

/// One file's worth of pending edit.
enum Change {
    /// A settings or MCP file in JSON.
    Json {
        path: PathBuf,
        settings: serde_json::Value,
    },
    /// The one harness that keeps its servers in TOML.
    Toml {
        path: PathBuf,
        document: toml_edit::DocumentMut,
    },
    /// A file anamnesis wrote whole.
    Delete { path: PathBuf },
}

impl Change {
    fn write(self) -> anyhow::Result<()> {
        match self {
            Self::Json { path, settings } => hooks::write_settings(&path, &settings),
            Self::Toml { path, document } => mcp_config::write_toml(&path, &document),
            Self::Delete { path } => {
                std::fs::remove_file(&path)?;
                Ok(())
            }
        }
    }
}

/// Where memory is, and what this command deliberately did not do to it.
fn report_memory(data: Option<&DataDir>) {
    println!();
    match data {
        Some(data) if data.root().exists() => {
            println!("  Memory is untouched, at {}:", data.root().display());
            println!("    `anamnesis purge --apply` removes this project's memory.");
            println!("    Deleting that directory removes all of it, for every project.");
        }
        _ => println!("  There is no memory on this machine to leave behind."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The obligation this command exists to keep. Somebody's own hook sits
    /// beside ours in the same file, and only ours may go.
    #[test]
    fn somebody_elses_hook_survives_the_uninstall() {
        let mut settings = json!({
            "hooks": {
                "PostToolUse": [
                    {"matcher": "*", "hooks": [{"type": "command", "command": "anamnesis hook --agent claude-code"}]},
                    {"matcher": "*", "hooks": [{"type": "command", "command": "./scripts/lint.sh"}]}
                ]
            }
        });

        let cleaned = hooks::remove_ours(&mut settings);

        assert_eq!(cleaned, ["PostToolUse"]);
        let left = settings["hooks"]["PostToolUse"].as_array().expect("array");
        assert_eq!(left.len(), 1, "{settings}");
        assert_eq!(left[0]["hooks"][0]["command"], "./scripts/lint.sh");
    }

    /// A wrapper somebody wrote that calls anamnesis is theirs. The predicate
    /// that decides is the installer's, so what may be removed is exactly what
    /// may be overwritten.
    #[test]
    fn a_wrapper_that_merely_calls_anamnesis_is_not_ours() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "./bin/start-memory-then-anamnesis.sh"}]}
                ]
            }
        });

        assert!(hooks::remove_ours(&mut settings).is_empty());
        assert!(settings["hooks"]["SessionStart"].is_array(), "{settings}");
    }

    /// A file that reads as configured when nothing is configured is the state
    /// at the bottom of every silent failure this project has had.
    #[test]
    fn nothing_is_left_looking_wired() {
        let mut settings = json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "anamnesis hook --agent codex"}]}
                ]
            },
            "otherSetting": true
        });

        hooks::remove_ours(&mut settings);

        assert!(
            settings.get("hooks").is_none(),
            "an empty hooks object was left behind: {settings}"
        );
        assert_eq!(settings["otherSetting"], true, "{settings}");
    }

    #[test]
    fn only_the_anamnesis_mcp_server_is_unregistered() {
        let mut config = json!({
            "mcpServers": {
                "anamnesis": {"command": "anamnesis", "args": ["mcp"]},
                "something-else": {"command": "other", "args": []}
            }
        });

        assert!(mcp_config::unregister(&mut config, mcp_config::SERVER_NAME));
        assert!(config["mcpServers"]["anamnesis"].is_null(), "{config}");
        assert_eq!(config["mcpServers"]["something-else"]["command"], "other");

        // And a second run finds nothing rather than failing.
        assert!(!mcp_config::unregister(
            &mut config,
            mcp_config::SERVER_NAME
        ));
    }

    #[test]
    fn an_mcp_file_holding_only_ours_does_not_read_as_configured_afterwards() {
        let mut config = json!({"mcpServers": {"anamnesis": {"command": "anamnesis"}}});

        assert!(mcp_config::unregister(&mut config, mcp_config::SERVER_NAME));

        assert!(config.get("mcpServers").is_none(), "{config}");
    }

    /// Codex keeps its servers in TOML, and the comments around them are
    /// somebody's. An uninstall that reformatted the file would take more than
    /// it was asked for.
    #[test]
    fn the_toml_file_keeps_everything_that_is_not_ours() {
        let source = "# my settings\nmodel = \"o3\"\n\n[mcp_servers.anamnesis]\ncommand = \"anamnesis\"\nargs = [\"mcp\"]\n\n[mcp_servers.other]\ncommand = \"other\"\n";
        let mut document: toml_edit::DocumentMut = source.parse().expect("toml");

        assert!(mcp_config::unregister_toml(
            &mut document,
            mcp_config::SERVER_NAME
        ));

        let written = document.to_string();
        assert!(written.contains("# my settings"), "{written}");
        assert!(written.contains("model = \"o3\""), "{written}");
        assert!(written.contains("[mcp_servers.other]"), "{written}");
        assert!(!written.contains("anamnesis"), "{written}");
    }
}
