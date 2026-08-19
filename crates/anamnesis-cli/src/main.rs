//! CLI entry point for anamnesis.

use anamnesis_core::datadir::DataDir;
use anamnesis_core::scope::{ScopeSource, resolve_scope};
use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "anamnesis")]
#[command(about = "Long-term memory for AI coding agents")]
#[command(version)]
#[command(long_about = "Anamnesis preserves context across AI agent sessions through a persistent wiki.
Quit Claude Code mid-task, start Codex in the same directory, and the next agent
receives a bounded handoff with previous decisions, attempted approaches, and open questions.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Root of the anamnesis data directory (wiki, raw, db, models, logs)
    #[arg(long, global = true, env = "ANAMNESIS_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Enable debug logging
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Show memory system status
    Status {
        /// Show detailed information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Search the memory wiki
    Search {
        /// Search query
        query: String,

        /// Limit number of results
        #[arg(short, long, default_value = "10")]
        limit: usize,

        /// Search only in specific path
        #[arg(long)]
        path: Option<String>,
    },

    /// Write or update a wiki page
    WritePage {
        /// Page path (e.g., decisions/0001-database.md)
        #[arg(long)]
        path: String,

        /// Page title
        #[arg(long)]
        title: String,

        /// Page body (markdown)
        #[arg(long)]
        body: String,

        /// Pin the page (prevent auto-decay)
        #[arg(long)]
        pinned: bool,

        /// Expiration date (YYYY-MM-DD)
        #[arg(long)]
        expires_at: Option<String>,
    },

    /// Create the data directory and register this project
    Init,

    /// Start the memory server
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Bind to address
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },

    /// Forward one lifecycle event, read as JSON on stdin
    ///
    /// Invoked by agent hooks, not by hand. Always exits 0: a memory system
    /// that can break someone's editing session is worse than one that
    /// occasionally misses an event.
    Hook {
        /// Which harness is calling
        #[arg(long, default_value = "claude-code")]
        agent: String,

        /// Server to deliver to
        #[arg(long, env = "ANAMNESIS_SERVER", default_value = "http://127.0.0.1:8080")]
        server: String,
    },

    /// Print the hook configuration to add to the agent's settings
    InstallHooks {
        /// Which harness to print configuration for
        #[arg(long, default_value = "claude-code")]
        agent: String,

        /// Server the hooks should deliver to
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,
    },

    /// Bootstrap from git history
    Bootstrap {
        /// Repository path
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },

    /// Show session handoff
    Handoff {
        /// Session ID
        session_id: String,
    },

    /// List all sessions
    Sessions {
        /// Limit number of results
        #[arg(short, long)]
        limit: Option<usize>,

        /// Sort order
        #[arg(long, default_value = "recent")]
        sort: String,
    },

    /// View page details
    ShowPage {
        /// Page path
        path: String,
    },

    /// Create a new session
    NewSession {
        /// Agent name (claude-code, codex, etc.)
        agent: String,

        /// Checkout path
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    if cli.debug {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    match cli.command {
        Commands::Status { verbose } => {
            cmd_status(verbose, cli.data_dir.clone())?;
        }
        Commands::Search { query, limit, path } => {
            cmd_search(&query, limit, path)?;
        }
        Commands::WritePage {
            path,
            title,
            body,
            pinned,
            expires_at,
        } => {
            cmd_write_page(&path, &title, &body, pinned, expires_at)?;
        }
        Commands::Init => {
            cmd_init(cli.data_dir.clone())?;
        }
        Commands::Serve { port, bind } => {
            cmd_serve(&bind, port, cli.data_dir.clone())?;
        }
        Commands::Hook { agent, server } => {
            cmd_hook(&agent, &server);
        }
        Commands::InstallHooks { agent, server } => {
            cmd_install_hooks(&agent, &server)?;
        }
        Commands::Bootstrap { repo } => {
            cmd_bootstrap(&repo)?;
        }
        Commands::Handoff { session_id } => {
            cmd_handoff(&session_id)?;
        }
        Commands::Sessions { limit, sort } => {
            cmd_sessions(limit, &sort)?;
        }
        Commands::ShowPage { path } => {
            cmd_show_page(&path)?;
        }
        Commands::NewSession { agent, path } => {
            cmd_new_session(&agent, path)?;
        }
    }

    Ok(())
}

fn cmd_status(verbose: bool, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    println!("📚 Anamnesis Memory Status");
    println!();
    println!("  Workspace: {}", scope.scope.workspace);
    println!("  Project:   {}", scope.scope.project);
    println!("  Identity:  {}", describe_source(&scope.source));

    if verbose {
        println!();
        println!("  Project key:  {}", scope.key);
        println!("  Project id:   {}", scope.project_id);
        println!("  Workspace id: {}", scope.workspace_id);
        println!("  Data dir:     {}", data.root().display());
        println!("  Wiki:         {}", data.wiki_scope(&scope.scope).display());
        println!("  Index:        {}", data.db_file().display());
        match &scope.marker {
            Some(path) => println!("  Marker:       {}", path.display()),
            None => println!("  Marker:       (none)"),
        }
    }

    if !data.root().exists() {
        println!();
        println!("  Not initialized yet — run `anamnesis init`.");
    }

    Ok(())
}

/// Explain, in one line, how the project was identified.
///
/// A wrong scope is the likeliest reason memory appears empty, so this is the
/// first thing `status` should make visible.
fn describe_source(source: &ScopeSource) -> String {
    match source {
        ScopeSource::Marker { path, legacy } => {
            let name = if *legacy {
                "legacy marker"
            } else {
                "marker"
            };
            format!("pinned by {name} at {}", path.display())
        }
        ScopeSource::GitRemote { normalized } => format!("git remote {normalized}"),
        ScopeSource::GitRoot { path } => format!("git working tree {}", path.display()),
        ScopeSource::CwdBasename { path } => {
            format!("directory name {}", path.display())
        }
    }
}

fn cmd_search(query: &str, limit: usize, path: Option<String>) -> anyhow::Result<()> {
    println!("🔍 Searching for: {}", query);
    if let Some(p) = path {
        println!("   in path: {}", p);
    }
    println!("   limit: {}", limit);
    println!();
    println!("(Search not yet implemented)");
    Ok(())
}

fn cmd_write_page(
    path: &str,
    title: &str,
    body: &str,
    pinned: bool,
    expires_at: Option<String>,
) -> anyhow::Result<()> {
    println!("✍️  Writing page: {}", path);
    println!("   Title: {}", title);
    if pinned {
        println!("   Pinned: yes");
    }
    if let Some(expires) = expires_at {
        println!("   Expires: {}", expires);
    }
    println!("   Body length: {} bytes", body.len());
    println!();
    println!("(Write not yet implemented)");
    Ok(())
}

fn cmd_init(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
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

fn cmd_serve(bind: &str, port: u16, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let data = DataDir::resolve(data_dir)?;
    data.ensure_layout()?;

    let store = Store::open(data.db_file())?;
    store.migrate()?;
    let wiki = Wiki::open(data.wiki())?;

    let address: std::net::SocketAddr = format!("{bind}:{port}").parse()?;
    println!("🌐 anamnesis serving on http://{address}");
    println!("   data dir: {}", data.root().display());
    println!("   POST /hook   GET /handoff   GET /health");

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(anamnesis_web::serve(
        address,
        anamnesis_web::AppState::new(store, wiki),
    ))?;
    Ok(())
}

/// Forward one hook event, and deliver the handoff when a session starts.
///
/// Never fails loudly. Hooks run inside someone's editing session, so a server
/// that is not running should cost them nothing more than a line on stderr.
fn cmd_hook(agent: &str, server: &str) {
    let mut payload = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload).is_err() {
        eprintln!("anamnesis: could not read hook payload");
        return;
    }

    let event = serde_json::from_str::<serde_json::Value>(&payload)
        .ok()
        .and_then(|value| {
            value
                .get("hook_event_name")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("anamnesis: {error}");
            return;
        }
    };

    let post = client
        .post(format!("{server}/hook"))
        .query(&[("agent", agent)])
        .header("content-type", "application/json")
        .body(payload.clone())
        .send();

    match post {
        Err(error) => {
            eprintln!("anamnesis: could not reach {server}: {error}");
            return;
        }
        // A refused event is still an event lost. Saying so on stderr costs the
        // session nothing and is the only way anyone finds out that capture
        // has quietly stopped working.
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            let detail = response.text().unwrap_or_default();
            eprintln!("anamnesis: server rejected event ({status}): {}", detail.trim());
            return;
        }
        Ok(_) => {}
    }

    // Only a starting session has anything to collect, and whatever comes back
    // goes to stdout, where the harness injects it into the model's context.
    if event.as_deref() == Some("SessionStart") {
        let (session_id, cwd) = session_and_cwd(&payload);
        let handoff = client
            .get(format!("{server}/handoff"))
            .query(&[
                ("agent", agent),
                ("session_id", &session_id),
                ("cwd", &cwd),
            ])
            .send()
            .and_then(reqwest::blocking::Response::text);

        match handoff {
            Ok(text) if !text.trim().is_empty() => print!("{text}"),
            Ok(_) => {}
            Err(error) => eprintln!("anamnesis: handoff unavailable: {error}"),
        }
    }
}

/// Pull the session id and working directory out of a hook payload.
fn session_and_cwd(payload: &str) -> (String, String) {
    let value: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let cwd = match field("cwd") {
        empty if empty.is_empty() => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        found => found,
    };
    (field("session_id"), cwd)
}

fn cmd_install_hooks(agent: &str, server: &str) -> anyhow::Result<()> {
    if agent != "claude-code" {
        println!("No hook template for {agent} yet.");
        return Ok(());
    }

    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "anamnesis".to_owned());
    let command = format!("{binary} hook --agent claude-code --server {server}");

    println!("Add this to your Claude Code settings.json:");
    println!();
    let events = [
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "PreCompact",
        "SessionEnd",
    ];
    let hooks: serde_json::Map<String, serde_json::Value> = events
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
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks }))?
    );
    println!();
    println!("Then start the server with `anamnesis serve`.");
    Ok(())
}

fn cmd_bootstrap(repo: &std::path::Path) -> anyhow::Result<()> {
    println!("📖 Bootstrapping from git history");
    println!("   repo: {}", repo.display());
    println!();
    println!("(Bootstrap not yet implemented)");
    Ok(())
}

fn cmd_handoff(session_id: &str) -> anyhow::Result<()> {
    println!("📋 Session handoff: {}", session_id);
    println!();
    println!("(Handoff not yet implemented)");
    Ok(())
}

fn cmd_sessions(limit: Option<usize>, sort: &str) -> anyhow::Result<()> {
    println!("📊 Sessions (sorted by {})", sort);
    if let Some(l) = limit {
        println!("   limit: {}", l);
    }
    println!();
    println!("(Sessions listing not yet implemented)");
    Ok(())
}

fn cmd_show_page(path: &str) -> anyhow::Result<()> {
    println!("📄 Page: {}", path);
    println!();
    println!("(Page display not yet implemented)");
    Ok(())
}

fn cmd_new_session(agent: &str, path: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    println!("🔄 Creating new session");
    println!("   agent: {}", agent);
    if let Some(p) = path {
        println!("   path: {}", p.display());
    }
    println!();
    println!("(Session creation not yet implemented)");
    Ok(())
}
