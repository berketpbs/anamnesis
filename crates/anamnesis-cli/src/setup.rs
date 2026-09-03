//! Getting anamnesis wired into a machine.
//!
//! Four commands, and each of them writes somewhere outside this project: a
//! data directory, a token, a harness's settings file. They are together
//! because they share the same obligation — to say exactly what they changed,
//! on disk, in a file somebody else's tool owns. A capture hook that was
//! registered wrongly is invisible until an afternoon is missing, which is
//! how this repository lost two days.

use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::format::describe_source;
use crate::{hooks, mcp_config, opencode};

/// Create this project's memory, and say where it went.
pub fn cmd_init(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    data.ensure_layout()?;
    let store = Store::open(data.db_file())?;
    store.migrate()?;
    store.upsert_project(&scope, Timestamp::now())?;

    println!("🚀 Initialized {}", scope.scope);
    println!();
    println!("  Data dir: {}", data.root().display());
    println!("  Wiki:     {}", data.wiki_scope(&scope.scope).display());
    println!("  Index:    {}", data.db_file().display());
    println!("  Identity: {}", describe_source(&scope.source));
    println!();
    println!("  {} project(s) registered.", store.project_count()?);

    Ok(())
}

/// Mint a token, and say where it goes.
///
/// Two variables rather than one, because they answer different questions.
/// `ANAMNESIS_TOKEN` is the secret this machine *presents*; `ANAMNESIS_TOKENS`
/// is the set a server *accepts*. On a single-user machine they hold the same
/// value and the distinction never comes up; on a shared server it is the
/// difference between "a token" and "whose token".
/// Mint a bearer token, optionally naming the operator it stands for.
pub fn cmd_token(operator: Option<&str>) -> anyhow::Result<()> {
    let secret = anamnesis_web::auth::generate_token()?;
    let token_env = anamnesis_web::auth::TOKEN_ENV;
    let tokens_env = anamnesis_web::auth::TOKENS_ENV;

    match operator {
        None => {
            println!("{secret}");
            println!();
            println!("  Server:  {token_env}={secret}");
            println!("  Client:  {token_env}={secret}");
            println!();
            println!("  Set it for the server and for whatever runs the hooks —");
            println!("  the same variable on both sides. Nothing was stored: this");
            println!("  is the only time it is printed.");
        }
        Some(name) => {
            // Validated here rather than at the server, so a name that could
            // never be accepted is refused while it is still a suggestion.
            let operator = anamnesis_core::scope::OperatorName::parse(name)?;
            println!("{secret}");
            println!();
            println!("  Server:  {tokens_env}={operator}={secret}");
            println!("  Client:  {token_env}={secret}");
            println!();
            println!("  Add the pair to the server's {tokens_env}, comma-separated");
            println!("  alongside any others. {operator}'s machine sets {token_env}.");
        }
    }

    Ok(())
}

/// Register the capture hooks with every harness on this machine.
/// Wire OpenCode, which takes a plugin rather than a command.
///
/// The shape of the report is deliberately the same as the settings path's:
/// where it went, what changed, and what to do next. What differs is the one
/// thing that has no equivalent on the other side — a file already at that
/// path that this command did not write is somebody's own plugin, and it is
/// left exactly where it is.
fn install_opencode_plugin(
    server: &str,
    write: bool,
    settings: Option<PathBuf>,
) -> anyhow::Result<()> {
    let binary = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "anamnesis".to_owned());
    let source = opencode::plugin(&binary, server);
    let path = match settings {
        Some(path) => path,
        None => opencode::plugin_path(&std::env::current_dir()?),
    };

    if !write {
        println!("OpenCode extends through a plugin, so this goes in a file of its own:");
        println!();
        println!("  {}", path.display());
        println!();
        println!("  It forwards the same five lifecycle events every other harness sends,");
        println!("  and pushes the waiting handoff into the system prompt — OpenCode has no");
        println!("  stdout channel for a hook to answer on.");
        println!();
        println!("Run this again with `--write` to put it there.");
        return Ok(());
    }

    match opencode::write(&path, &source)? {
        opencode::Written::Foreign => {
            println!("🪝 {}", path.display());
            println!();
            println!("  A plugin is already there and anamnesis did not write it.");
            println!("  Nothing was changed: whatever else that file does is somebody's,");
            println!("  and this command has no way to keep it.");
            println!();
            println!("  Move it aside and run this again, or add the anamnesis hooks to it");
            println!("  by hand — `--write --settings <path>` writes ours anywhere else.");
            return Ok(());
        }
        opencode::Written::Created => {
            println!("🪝 {}", path.display());
            println!();
            println!("  Written.      the five lifecycle events, and the handoff");
        }
        opencode::Written::Rewritten => {
            println!("🪝 {}", path.display());
            println!();
            println!("  Rewritten.    it pointed at a different binary or server");
        }
        opencode::Written::Unchanged => {
            println!("🪝 {}", path.display());
            println!();
            println!("  Already this. nothing to do");
        }
    }

    println!("  Binary:       {binary}");
    println!("  Server:       {server}");
    println!();
    println!("  OpenCode loads plugins at startup, so this takes effect in the next");
    println!("  session. Start the server with `anamnesis serve`.");
    println!();
    println!(
        "  If that server requires a token, set {} in the environment OpenCode",
        anamnesis_web::auth::TOKEN_ENV
    );
    println!("  starts from: the plugin passes it on and the secret stays out of the file.");
    Ok(())
}

pub fn cmd_install_hooks(
    agent: &str,
    server: &str,
    write: bool,
    settings: Option<PathBuf>,
) -> anyhow::Result<()> {
    // OpenCode is wired by writing a module, not by merging a command into a
    // settings file, so it comes apart from the rest before anything else does.
    if agent == "opencode" {
        return install_opencode_plugin(server, write, settings);
    }

    let Some(harness) = hooks::harness(agent) else {
        println!("No hook template for {agent} yet.");
        println!();
        println!(
            "  Wired today: {}, opencode",
            hooks::HARNESSES
                .iter()
                .map(|harness| harness.agent)
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(());
    };

    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "anamnesis".to_owned());
    let config = hooks::hook_config(&harness, &hooks::hook_command(&binary, agent, server));

    if !write {
        println!(
            "Add this to {}:",
            hooks::default_settings_path(&harness, std::path::Path::new(".")).display()
        );
        println!();
        println!("{}", serde_json::to_string_pretty(&config)?);
        println!();
        println!("Or run this again with `--write` to merge it in for you.");
        println!("Then start the server with `anamnesis serve`.");
        println!();
        println!(
            "If that server requires a token, set {} in the environment",
            anamnesis_web::auth::TOKEN_ENV
        );
        println!("the harness starts from. Hooks inherit it, and the secret stays");
        println!("out of the settings file.");
        return Ok(());
    }

    let path = match settings {
        Some(path) => path,
        None => hooks::default_settings_path(&harness, &std::env::current_dir()?),
    };

    // Read failures stop here rather than starting a fresh file over the top of
    // one we could not parse. Printing the configuration leaves the person a
    // minute of pasting; the alternative costs them their editor settings.
    let mut existing = match hooks::read_settings(&path) {
        Ok(settings) => settings,
        Err(error) => {
            println!("Could not read {} — {error}", path.display());
            println!();
            println!("Nothing was changed. Add this by hand:");
            println!();
            println!("{}", serde_json::to_string_pretty(&config)?);
            return Ok(());
        }
    };

    let outcome = hooks::merge(&mut existing, &config);
    if outcome.changed() {
        hooks::write_settings(&path, &existing)?;
    }

    println!("🪝 {}", path.display());
    println!();
    if !outcome.added.is_empty() {
        println!("  Wired:        {}", outcome.added.join(", "));
    }
    // Named separately from "wired", because the file already looked wired and
    // was not: an anamnesis command that needed rewriting is the shape a
    // silently broken capture takes, and it is worth seeing that it was there.
    if !outcome.replaced.is_empty() {
        println!("  Rewritten:    {}", outcome.replaced.join(", "));
    }
    if !outcome.present.is_empty() {
        println!("  Already there: {}", outcome.present.join(", "));
    }
    if !outcome.changed() {
        println!();
        println!("  Nothing to do — every event was already delivering here.");
        return Ok(());
    }

    println!();
    println!("  Delivering to {server}.");
    // Said rather than assumed: hooks are read when a session starts, so the
    // session running this command is not the one that will be captured.
    println!("  Takes effect in the next session, not this one.");
    println!("  {}", harness.note);
    println!("  Start the server with `anamnesis serve`, then check with");
    println!("  `anamnesis status`.");
    // The command written into the file carries no secret, on purpose: a
    // settings file is read aloud by the harness and copied between machines.
    if std::env::var_os(anamnesis_web::auth::TOKEN_ENV).is_none() {
        println!();
        println!(
            "  If that server requires a token, set {} in the environment",
            anamnesis_web::auth::TOKEN_ENV
        );
        println!("  the harness starts from — not in this file.");
    }
    Ok(())
}

/// Register the MCP server with a harness.
///
/// The counterpart to `install-hooks`, and it exists for the same reason that
/// one does. Connecting an agent takes two steps; only one of them had a
/// command, and the other was a line of documentation that assumed the binary
/// was on `PATH`. On the machine this project is developed on it is not — it is
/// copied out of `target/` so that `cargo build` can overwrite it — so the
/// documented line would have registered a server that cannot start. It was
/// never run at all: hooks recorded four months of sessions the agent could not
/// search.
/// Register the MCP server with every harness on this machine.
/// What a registered MCP server should be started with, and what to say about
/// it.
///
/// The vector stream is the one part of retrieval that lives in the process
/// asking the question rather than in the index. A registration that does not
/// carry it produces a server that answers every `memory_query` with three of
/// the four streams — over pages whose vectors are sitting in the index,
/// written by a server that *was* configured for it. Measured here before this
/// was written: an English question about a Turkish page came back without the
/// page, and came back with it at rank three the moment the same query ran
/// with the stream on.
///
/// The key of a hosted embedder is deliberately not carried. A secret in a
/// settings file is a different decision from a setting in one, and this
/// command is not the place to make it on somebody's behalf.
fn mcp_environment() -> (Vec<(String, String)>, Option<String>) {
    let config = anamnesis_llm::EmbedConfig::from_env();
    if !config.enabled {
        return (Vec::new(), None);
    }

    if config.provider == anamnesis_llm::embed::EmbedProvider::Hosted {
        return (
            Vec::new(),
            Some(format!(
                "Embeddings are hosted ({}), so the registration carries none of it:\n  \
                 the endpoint wants a key, and a key belongs in the environment the\n  \
                 harness starts in, not in a settings file. Without it the agent's\n  \
                 queries run without the vector stream.",
                config.model
            )),
        );
    }

    let mut env = vec![("ANAMNESIS_EMBED_ENABLED".to_owned(), "1".to_owned())];
    if config.model != anamnesis_llm::embed::DEFAULT_MODEL {
        env.push(("ANAMNESIS_EMBED_MODEL".to_owned(), config.model.clone()));
    }
    let said = format!("Vectors: {} — carried into the registration", config.model);
    (env, Some(said))
}

pub fn cmd_install_mcp(
    agent: &str,
    write: bool,
    config_path: Option<PathBuf>,
    repo: Option<PathBuf>,
) -> anyhow::Result<()> {
    let Some(target) = mcp_config::target(agent) else {
        // A harness that cannot be registered this way gets a reason rather
        // than a "not yet", the same as `install-hooks`: one of them is a
        // permanent difference in how it extends.
        match mcp_config::cannot_register(agent) {
            Some(reason) => {
                println!("{agent} cannot be registered by install-mcp.");
                println!();
                println!("  {reason}");
            }
            None => {
                println!("No MCP template for {agent} yet.");
                println!();
                println!(
                    "  Registered today: {}",
                    mcp_config::TARGETS
                        .iter()
                        .map(|target| target.agent)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!("  The server itself is harness-agnostic: `anamnesis mcp --repo <dir>`");
                println!("  over stdio is all any of them need.");
            }
        }
        return Ok(());
    };

    let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("anamnesis"));
    let repo = match repo {
        Some(repo) => repo,
        None => std::env::current_dir()?,
    };
    let (env, embedding) = mcp_environment();
    let entry = mcp_config::server_entry(&binary, &repo, &env);

    if !write {
        println!(
            "Add this to {}:",
            mcp_config::config_path(&target, std::path::Path::new(".")).display()
        );
        println!();
        println!("{}", mcp_config::render(&target, &entry)?);
        println!();
        println!("Or run this again with `--write` to merge it in for you.");
        println!();
        println!("The MCP server does not need `anamnesis serve`: it opens the");
        println!("store directly. Hooks are the half that needs the server.");
        return Ok(());
    }

    let path = match config_path {
        Some(path) => path,
        None => mcp_config::config_path(&target, &std::env::current_dir()?),
    };

    let outcome = match mcp_config::apply(&target, &path, mcp_config::SERVER_NAME, &entry) {
        Ok(outcome) => outcome,
        // Same rule for both shapes: a file that exists and does not parse is
        // somebody's configuration and possibly the only copy of it.
        Err(error) => {
            println!("Could not read {} — {error}", path.display());
            println!();
            println!("Nothing was changed. Add this by hand:");
            println!();
            println!("{}", mcp_config::render(&target, &entry)?);
            return Ok(());
        }
    };

    println!("🔌 {}", path.display());
    println!();
    match &outcome {
        mcp_config::Registration::Added => {
            println!("  Registered:   {}", mcp_config::describe(&entry));
        }
        mcp_config::Registration::Replaced(previous) => {
            println!("  Registered:   {}", mcp_config::describe(&entry));
            println!("  Replaced:     {previous}");
        }
        mcp_config::Registration::Unchanged => {
            println!("  Already registered: {}", mcp_config::describe(&entry));
            println!();
            println!("  Nothing to do.");
            return Ok(());
        }
    }

    if let Some(embedding) = &embedding {
        println!("  {embedding}");
    }

    println!();
    println!("  Takes effect in the next session, not this one.");
    println!("  Then the agent can call memory_query rather than waiting to be");
    println!("  handed one summary at startup.");
    println!("  {}", target.note);
    println!();
    // Said because these files are ones a project may commit, and what is in
    // them is this machine's: an absolute path to a binary nobody else has.
    println!("  This names paths on this machine. Ignore the file, or expect a");
    println!("  colleague's checkout to point at your home directory.");
    Ok(())
}
