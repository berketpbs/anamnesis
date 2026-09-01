//! Starting a harness with memory wired, and refusing when it is not.
//!
//! Everything else in anamnesis is careful about *recording* a session. This
//! is about the minute before one starts, and it exists because of the only
//! failure this system has had twice: an afternoon of work that reached
//! nothing, discovered days later. Both times the cause was ordinary — a
//! server that was not running, then a server too old to read the marker file
//! — and both times the session had already happened by the time anybody
//! looked.
//!
//! So `anamnesis run` checks first and starts second. If the server cannot be
//! reached, or the harness has no hooks wired to it, nothing is launched: a
//! session that will not be recorded is exactly the session worth not starting
//! yet. `--anyway` says otherwise, for the times when the work matters more
//! than the record of it.
//!
//! `anamnesis continue` is the same launch, aimed at whichever harness this
//! project last used. The memory travels either way — the handoff is waiting
//! for whoever starts next, and every harness reads the same one — so this is
//! about picking up a thread rather than about moving memory between tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anamnesis_store::Store;

use crate::hooks;
use crate::opencode;
use crate::project::open_project;

/// What a harness is called on the command line.
///
/// A guess that is wrong is visible immediately — the program is not found,
/// and the message says which name was tried and how to override it — which is
/// the failure mode a launcher can afford. `--program` is the override.
pub fn program_for(agent: &str) -> Option<&'static str> {
    Some(match agent {
        "claude-code" => "claude",
        "codex" => "codex",
        "cursor" => "cursor-agent",
        "gemini-cli" => "gemini",
        "opencode" => "opencode",
        _ => return None,
    })
}

/// Whether this project has hooks wired for a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wiring {
    /// The settings file, or the plugin, points at anamnesis.
    Wired,
    /// The file exists but nothing in it mentions anamnesis.
    Unwired,
    /// There is no file at all.
    Missing,
}

/// What starting this session would be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Start it.
    Go,
    /// Start it, but say what will not be recorded.
    GoAnyway(String),
    /// Do not start it, and say what to do instead.
    Stop {
        /// What is wrong, in a sentence.
        reason: String,
        /// The command that fixes it.
        fix: String,
    },
}

/// Decide whether a session should start.
///
/// Pure on purpose: the interesting part is the decision, and a decision made
/// inside a function that also spawns a process is one nothing can test.
pub fn decide(
    server_up: bool,
    server: &str,
    wiring: Wiring,
    agent: &str,
    anyway: bool,
) -> Decision {
    let trouble = if !server_up {
        Some((
            format!("the memory server at {server} is not answering"),
            "anamnesis serve".to_owned(),
        ))
    } else {
        match wiring {
            Wiring::Wired => None,
            Wiring::Missing => Some((
                format!("{agent} has no anamnesis hooks in this project"),
                format!("anamnesis install-hooks --agent {agent} --write"),
            )),
            Wiring::Unwired => Some((
                format!("{agent}'s hooks are configured, but not to anamnesis"),
                format!("anamnesis install-hooks --agent {agent} --write"),
            )),
        }
    };

    match (trouble, anyway) {
        (None, _) => Decision::Go,
        // Said as what it costs, not as a warning about a setting: the thing
        // being given up is the record of the next few hours.
        (Some((reason, _)), true) => Decision::GoAnyway(format!(
            "{reason} — nothing from this session will be remembered"
        )),
        (Some((reason, fix)), false) => Decision::Stop { reason, fix },
    }
}

/// How a harness's hooks look in this project.
pub fn wiring_for(agent: &str, root: &Path, binary_hint: &str) -> Wiring {
    if agent == "opencode" {
        let path = opencode::plugin_path(root);
        return match std::fs::read_to_string(&path) {
            Ok(text) if text.contains("anamnesis") => Wiring::Wired,
            Ok(_) => Wiring::Unwired,
            Err(_) => Wiring::Missing,
        };
    }

    let Some(harness) = hooks::harness(agent) else {
        return Wiring::Missing;
    };
    let path = hooks::default_settings_path(&harness, root);
    match std::fs::read_to_string(&path) {
        // Looked for by name rather than by parsing: what matters is whether
        // this project's hooks reach anamnesis at all, and a file shaped in a
        // way this build does not expect is still a file somebody wired.
        Ok(text) if text.contains("anamnesis") && text.contains(binary_hint) => Wiring::Wired,
        Ok(text) if text.contains("anamnesis") => Wiring::Wired,
        Ok(_) => Wiring::Unwired,
        Err(_) => Wiring::Missing,
    }
}

/// The command a launch will run, with the environment memory needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The harness's executable, as it will be looked up on `PATH`.
    pub program: String,
    /// Everything after `--`, passed through untouched.
    pub args: Vec<String>,
    /// What the harness — and therefore its hooks — inherits.
    pub env: BTreeMap<String, String>,
}

/// Build the launch for a harness.
///
/// The server address goes into the environment rather than into the hook
/// command, so that a project wired to one server can be run against another
/// without rewriting anybody's settings file. The token is passed on only when
/// this process has one: putting an empty variable into the environment would
/// make a harness present an empty credential to a server that accepts none.
pub fn launch(program: &str, args: &[String], server: &str, token: Option<&str>) -> Launch {
    let mut env = BTreeMap::new();
    env.insert("ANAMNESIS_SERVER".to_owned(), server.to_owned());
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        env.insert(anamnesis_web::auth::TOKEN_ENV.to_owned(), token.to_owned());
    }
    Launch {
        program: program.to_owned(),
        args: args.to_vec(),
        env,
    }
}

/// The harness this project used most recently, if it has used one.
pub fn last_agent(store: &Store, project: anamnesis_core::ids::ProjectId) -> Option<String> {
    store
        .recent_sessions(project, 1)
        .ok()?
        .into_iter()
        .next()
        .map(|session| session.agent)
}

/// Start a harness in this project.
pub fn cmd_run(
    agent: &str,
    program: Option<String>,
    args: &[String],
    server: &str,
    token: Option<&str>,
    anyway: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, _data, _store) = open_project(data_dir)?;
    start(agent, program, args, server, token, anyway, &scope.root)
}

/// Start whichever harness this project last used.
pub fn cmd_continue(
    program: Option<String>,
    args: &[String],
    server: &str,
    token: Option<&str>,
    anyway: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;
    let Some(agent) = last_agent(&store, scope.project_id) else {
        println!("Nothing has been recorded in {} yet.", scope.scope);
        println!();
        println!("  There is no session to continue, so there is no harness to pick.");
        println!("  Start one by name:");
        println!();
        println!("    anamnesis run claude-code");
        return Ok(());
    };

    println!("↩  Continuing with {agent}, which ran the last session here.");
    println!();
    start(&agent, program, args, server, token, anyway, &scope.root)
}

/// The half both commands share: check, report, launch.
fn start(
    agent: &str,
    program: Option<String>,
    args: &[String],
    server: &str,
    token: Option<&str>,
    anyway: bool,
    root: &Path,
) -> anyhow::Result<()> {
    let Some(default_program) = program_for(agent) else {
        anyhow::bail!("no launcher for {agent}; pass --program <executable> to say what to run");
    };
    let program = program.unwrap_or_else(|| default_program.to_owned());

    let binary = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "anamnesis".to_owned());
    let wiring = wiring_for(agent, root, &binary);

    match decide(server_reachable(server), server, wiring, agent, anyway) {
        Decision::Stop { reason, fix } => {
            println!("⏸  Not starting {agent}: {reason}.");
            println!();
            println!("  Everything this session did would be lost the way two afternoons");
            println!("  in this repository already were — silently, and discovered later.");
            println!();
            println!("    {fix}");
            println!();
            println!("  Or `--anyway` to start without a memory of it.");
            return Ok(());
        }
        Decision::GoAnyway(notice) => {
            println!("⚠  {notice}.");
            println!();
        }
        Decision::Go => {}
    }

    let launch = launch(&program, args, server, token);
    let mut command = std::process::Command::new(&launch.program);
    command.args(&launch.args);
    for (key, value) in &launch.env {
        command.env(key, value);
    }

    let status = match command.status() {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "could not find {} on PATH — pass --program <executable> if {agent} is called \
                 something else here",
                launch.program
            );
        }
        Err(error) => anyhow::bail!("could not start {}: {error}", launch.program),
    };

    // The harness's own exit code, because this command is a launcher and a
    // launcher that swallows one breaks every script that wraps it.
    if !status.success()
        && let Some(code) = status.code()
    {
        std::process::exit(code);
    }
    Ok(())
}

/// Whether the memory server answers.
fn server_reachable(server: &str) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    matches!(
        client.get(format!("{server}/health")).send(),
        Ok(response) if response.status().is_success()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What is typed after the harness has to arrive at the harness. This is
    /// the whole difference between a launcher and a wrapper that eats your
    /// arguments, and it is decided by clap rather than by anything here.
    #[test]
    fn what_is_typed_after_the_harness_reaches_the_harness() {
        use clap::Parser;

        let parsed = crate::cli::Cli::try_parse_from([
            "anamnesis",
            "run",
            "claude-code",
            "--program",
            "cmd",
            "--",
            "/c",
            "echo",
            "hi",
        ])
        .expect("parse");

        let crate::cli::Commands::Run { agent, args, .. } = parsed.command else {
            panic!("not a run");
        };
        assert_eq!(agent, "claude-code");
        assert_eq!(args, ["/c", "echo", "hi"]);
    }

    #[test]
    fn every_wired_harness_has_something_to_launch() {
        for harness in hooks::HARNESSES {
            assert!(
                program_for(harness.agent).is_some(),
                "{} has hooks but no launcher",
                harness.agent
            );
        }
        assert!(program_for("opencode").is_some());
        assert!(program_for("something-else").is_none());
    }

    /// The whole point of the command. A session that will not be recorded is
    /// the session worth not starting, because the cost is discovered days
    /// later and cannot be paid back.
    #[test]
    fn a_session_that_would_not_be_recorded_does_not_start() {
        let stopped = decide(
            false,
            "http://127.0.0.1:8080",
            Wiring::Wired,
            "codex",
            false,
        );

        let Decision::Stop { reason, fix } = stopped else {
            panic!("a session started against a server that is not there: {stopped:?}");
        };
        assert!(reason.contains("not answering"), "{reason}");
        assert_eq!(fix, "anamnesis serve");
    }

    #[test]
    fn a_harness_with_no_hooks_is_told_which_command_wires_it() {
        let stopped = decide(true, "http://x", Wiring::Missing, "cursor", false);

        let Decision::Stop { fix, .. } = stopped else {
            panic!("an unwired harness started: {stopped:?}");
        };
        assert_eq!(fix, "anamnesis install-hooks --agent cursor --write");
    }

    /// Hooks that exist and point somewhere else are their own failure: the
    /// file looks configured, and nothing is being recorded.
    #[test]
    fn hooks_wired_to_something_else_are_not_wired_to_anamnesis() {
        let stopped = decide(true, "http://x", Wiring::Unwired, "codex", false);

        let Decision::Stop { reason, .. } = stopped else {
            panic!("a session started with somebody else's hooks: {stopped:?}");
        };
        assert!(reason.contains("not to anamnesis"), "{reason}");
    }

    /// And the escape hatch says what it costs rather than warning about a
    /// setting.
    #[test]
    fn anyway_starts_and_says_what_is_being_given_up() {
        let Decision::GoAnyway(notice) = decide(false, "http://x", Wiring::Wired, "codex", true)
        else {
            panic!("--anyway did not start the session");
        };
        assert!(notice.contains("nothing from this session will be remembered"));
    }

    #[test]
    fn a_healthy_setup_starts_without_saying_anything() {
        assert_eq!(
            decide(true, "http://x", Wiring::Wired, "claude-code", false),
            Decision::Go
        );
    }

    /// The address travels in the environment, not in the hook command, so a
    /// project wired to one server can be run against another without
    /// rewriting anybody's settings file.
    #[test]
    fn the_launch_carries_the_server_and_the_token() {
        let launched = launch(
            "claude",
            &["--resume".to_owned()],
            "http://memory.example.com",
            Some("anam_secret"),
        );

        assert_eq!(launched.program, "claude");
        assert_eq!(launched.args, ["--resume"]);
        assert_eq!(
            launched.env.get("ANAMNESIS_SERVER").map(String::as_str),
            Some("http://memory.example.com")
        );
        assert_eq!(
            launched
                .env
                .get(anamnesis_web::auth::TOKEN_ENV)
                .map(String::as_str),
            Some("anam_secret")
        );
    }

    /// An empty variable is worse than none: it makes a harness present an
    /// empty credential to a server that accepts none, which is a rejection
    /// where there was no problem.
    #[test]
    fn no_token_means_no_variable() {
        let launched = launch("codex", &[], "http://x", None);
        assert!(!launched.env.contains_key(anamnesis_web::auth::TOKEN_ENV));

        let empty = launch("codex", &[], "http://x", Some(""));
        assert!(!empty.env.contains_key(anamnesis_web::auth::TOKEN_ENV));
    }

    #[test]
    fn wiring_is_read_from_the_file_each_harness_actually_uses() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert_eq!(
            wiring_for("claude-code", dir.path(), "anamnesis.exe"),
            Wiring::Missing
        );

        let settings = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(settings.parent().expect("parent")).expect("dirs");
        std::fs::write(&settings, r#"{"hooks":{"SessionStart":[]}}"#).expect("write");
        assert_eq!(
            wiring_for("claude-code", dir.path(), "anamnesis.exe"),
            Wiring::Unwired
        );

        std::fs::write(
            &settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"anamnesis hook"}]}]}}"#,
        )
        .expect("write");
        assert_eq!(
            wiring_for("claude-code", dir.path(), "anamnesis.exe"),
            Wiring::Wired
        );
    }

    /// OpenCode keeps its wiring in a plugin file rather than a settings file,
    /// and the check has to look where the thing actually is.
    #[test]
    fn opencode_wiring_is_read_from_its_plugin() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            wiring_for("opencode", dir.path(), "anamnesis"),
            Wiring::Missing
        );

        let path = opencode::plugin_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, "export const Mine = async () => ({});\n").expect("theirs");
        assert_eq!(
            wiring_for("opencode", dir.path(), "anamnesis"),
            Wiring::Unwired
        );

        std::fs::write(&path, opencode::plugin("anamnesis", "http://x")).expect("ours");
        assert_eq!(
            wiring_for("opencode", dir.path(), "anamnesis"),
            Wiring::Wired
        );
    }
}
