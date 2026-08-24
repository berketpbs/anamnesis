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

    /// Start the MCP server over stdio, for an agent harness to launch as a
    /// subprocess
    Mcp {
        /// Repository or directory whose scope the server should resolve
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },

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

    /// Show the handoff waiting for the next session, without consuming it
    Handoff {
        /// Workstream to look in. Omitted shows the project-wide handoff.
        #[arg(long)]
        workstream: Option<String>,
    },

    /// List recent sessions, newest first
    Sessions {
        /// Limit number of results
        #[arg(short, long)]
        limit: Option<usize>,
    },

    /// View page details
    ShowPage {
        /// Page path
        path: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Logging is always on, at info, because consolidation now happens after
    // the response has been sent — a model that refuses, times out, or answers
    // with nonsense leaves no trace anywhere else, and the session it belonged
    // to has already gone.
    //
    // To stderr, not stdout: the `hook` command writes the next session's
    // handoff to stdout, and the harness injects whatever appears there into
    // the model context. A log line on that stream would become part of the
    // agent memory it was describing.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(if cli.debug { "debug" } else { "info" })
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Commands::Status { verbose } => {
            cmd_status(verbose, cli.data_dir.clone())?;
        }
        Commands::Search { query, limit, path } => {
            cmd_search(&query, limit, path, cli.data_dir.clone())?;
        }
        Commands::WritePage {
            path,
            title,
            body,
            pinned,
            expires_at,
        } => {
            cmd_write_page(&path, &title, &body, pinned, expires_at, cli.data_dir.clone())?;
        }
        Commands::Init => {
            cmd_init(cli.data_dir.clone())?;
        }
        Commands::Mcp { repo } => {
            cmd_mcp(&repo, cli.data_dir.clone())?;
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
        Commands::Handoff { workstream } => {
            cmd_handoff(workstream, cli.data_dir.clone())?;
        }
        Commands::Sessions { limit } => {
            cmd_sessions(limit, cli.data_dir.clone())?;
        }
        Commands::ShowPage { path } => {
            cmd_show_page(&path, cli.data_dir.clone())?;
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

        // Reported here because the alternative is finding out from a page
        // footer, a week later, that every summary was written by counting.
        match anamnesis_llm::LlmConfig::from_env() {
            Ok(llm) if llm.provider == anamnesis_llm::ProviderKind::None => {
                println!("  Model:        (none — summaries are counted)");
            }
            Ok(llm) => println!("  Model:        {}", llm.model),
            Err(error) => println!("  Model:        misconfigured — {error}"),
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

/// Open the index and wiki for the project containing the current directory.
///
/// Every read-only command needs the same three things, and getting them
/// wrong (a data dir that does not exist, a scope resolved from the wrong
/// directory) is the usual reason a command reports nothing rather than
/// failing outright.
fn open_project(data_dir: Option<PathBuf>) -> anyhow::Result<(anamnesis_core::scope::ResolvedScope, DataDir, Store)> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;
    if !data.db_file().exists() {
        anyhow::bail!(
            "no memory at {} — run `anamnesis init` first",
            data.root().display()
        );
    }
    let store = Store::open(data.db_file())?;
    store.migrate()?;
    Ok((scope, data, store))
}

fn cmd_search(
    query: &str,
    limit: usize,
    path: Option<String>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;

    // The same opt-in local embedder `anamnesis mcp` uses, so a search from
    // the terminal ranks identically to one an agent runs.
    let embedder = anamnesis_llm::EmbedConfig::from_env().build(&data.models())?;
    let query_vector = embedder.as_ref().and_then(|embedder| match embedder.embed(query) {
        Ok(vector) => Some((embedder.model().to_owned(), vector)),
        Err(error) => {
            eprintln!("anamnesis: query embedding failed ({error}); searching without it");
            None
        }
    });

    let hits = store.query_pages(
        scope.project_id,
        query,
        limit,
        Timestamp::now(),
        query_vector
            .as_ref()
            .map(|(model, vector)| (model.as_str(), vector.as_slice())),
    )?;

    let hits: Vec<_> = match &path {
        Some(prefix) => hits
            .into_iter()
            .filter(|hit| hit.path.as_str().starts_with(prefix.as_str()))
            .collect(),
        None => hits,
    };

    if hits.is_empty() {
        println!("No pages matched {query:?}.");
        return Ok(());
    }

    for hit in &hits {
        let mut marks = Vec::new();
        if hit.pinned {
            marks.push("pinned");
        }
        if hit.canonical {
            marks.push("canonical");
        }
        if !hit.status.is_answerable() {
            marks.push(hit.status.as_str());
        }
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!(" [{}]", marks.join(", "))
        };

        println!("{}  {}{}", hit.path, hit.title, marks);
        println!("    {} · score {:.4}", hit.tier.as_str(), hit.score);
        if !hit.snippet.is_empty() {
            println!("    {}", hit.snippet.replace('\n', " "));
        }
        println!();
    }
    Ok(())
}

fn cmd_write_page(
    path: &str,
    title: &str,
    body: &str,
    pinned: bool,
    expires_at: Option<String>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let wiki = Wiki::open(data.wiki())?;

    let page_path = anamnesis_core::page::PagePath::parse(path)?;
    let mut frontmatter = anamnesis_core::page::Frontmatter::new(title, Vec::new())?;
    frontmatter.pinned = pinned;
    if let Some(expires) = &expires_at {
        // Accepting a bare date is the whole reason this is parsed here rather
        // than deserialized: `--expires-at 2026-12-31` is what someone types,
        // and rejecting it for want of a time of day would be pedantry.
        let stamp = if expires.len() == 10 {
            format!("{expires}T00:00:00Z")
        } else {
            expires.clone()
        };
        frontmatter.expires_at = Some(
            stamp
                .parse()
                .map_err(|_| anyhow::anyhow!("--expires-at {expires:?} is not a date or RFC 3339 timestamp"))?,
        );
    }

    let now = Timestamp::now();
    store.upsert_project(&scope, now)?;

    let mut page = anamnesis_core::page::Page::new(scope.project_id, page_path.clone(), frontmatter, body);
    let commit = wiki.write_page(&scope.scope, &page, &format!("cli: write {page_path}"))?;
    page.git_commit = Some(commit.clone());

    store.upsert_page(&page, now)?;
    store.set_page_links(scope.project_id, page.id, &anamnesis_wiki::extract_links(body))?;

    println!("✍️  Wrote {page_path}");
    println!("   {}", wiki.locate(&scope.scope, &page_path).display());
    println!("   commit {}", &commit[..commit.len().min(8)]);
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

/// Start the MCP server bound to `repo`'s scope, speaking stdio.
///
/// One process per project: the scope is resolved once, at startup, the same
/// way `serve` binds one store and wiki rather than re-resolving per request.
/// A harness that wants a different project starts a different process.
fn cmd_mcp(repo: &std::path::Path, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = repo
        .canonicalize()
        .unwrap_or_else(|_| repo.to_path_buf());
    let scope = resolve_scope(&repo)?;
    let data = DataDir::resolve(data_dir)?;
    data.ensure_layout()?;

    let store = Store::open(data.db_file())?;
    store.migrate()?;
    store.upsert_project(&scope, Timestamp::now())?;
    let wiki = Wiki::open(data.wiki())?;

    // Built before the transport connects, so a misconfigured or unreachable
    // model is a startup error someone sees rather than a warning buried in a
    // log file, the same reasoning `cmd_serve` applies to the LLM provider.
    let embed_config = anamnesis_llm::EmbedConfig::from_env();
    let embedder = embed_config.build(&data.models())?;

    // Never stdout: the MCP transport owns stdout for protocol frames, so a
    // stray print here would corrupt the stream the same way a log line would
    // corrupt the `hook` command's handoff channel.
    eprintln!(
        "anamnesis: mcp server for {} ({})",
        scope.scope,
        describe_source(&scope.source)
    );
    eprintln!(
        "   vector search: {}",
        match &embedder {
            Some(embedder) => format!("enabled ({})", embedder.model()),
            None => "disabled (set ANAMNESIS_EMBED_ENABLED=1 to turn on)".to_owned(),
        }
    );

    let server = anamnesis_mcp::AnamnesisMcp::new(store, wiki, scope, repo).with_embedder(embedder);

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        use rmcp::ServiceExt;
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        anyhow::Ok(())
    })?;
    Ok(())
}

fn cmd_serve(bind: &str, port: u16, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let data = DataDir::resolve(data_dir)?;
    data.ensure_layout()?;

    let store = Store::open(data.db_file())?;
    store.migrate()?;
    let wiki = Wiki::open(data.wiki())?;

    // Built before the listener binds, so a misconfigured model is a startup
    // error someone sees rather than a warning that only surfaces hours later,
    // after sessions have already been summarised without one.
    let llm = anamnesis_llm::LlmConfig::from_env()?;
    let settings = llm.build()?.map(|provider| anamnesis_web::LlmSettings {
        provider,
        max_input_tokens: llm.max_input_tokens,
        max_output_tokens: llm.max_output_tokens,
    });

    let address: std::net::SocketAddr = format!("{bind}:{port}").parse()?;
    println!("🌐 anamnesis serving on http://{address}");
    println!("   data dir: {}", data.root().display());
    println!("   POST /hook   GET /handoff   GET /health");
    match &settings {
        Some(settings) => println!(
            "   consolidation: {} ({})",
            settings.provider.model(),
            settings.provider.name()
        ),
        None => println!("   consolidation: counted (no model configured)"),
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(anamnesis_web::serve(
        address,
        anamnesis_web::AppState::new(store, wiki).with_llm(settings),
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

    // Windows shells prepend a UTF-8 byte order mark when piping text into a
    // native process, and a BOM is not valid JSON. Stripping it here means the
    // hook works the same whether it was invoked from PowerShell, cmd, or a
    // POSIX shell.
    let payload = payload.trim_start_matches('\u{feff}').trim().to_owned();
    if payload.is_empty() {
        eprintln!("anamnesis: hook payload was empty");
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

    // These budgets are deliberately tight. A hook runs before every tool call
    // an agent makes, so any delay here is multiplied by hundreds within one
    // session — and the case that matters is the server being *down*, where a
    // generous timeout turns "memory is not running" into "the agent feels
    // broken". Losing an event costs a line in a summary; stalling the session
    // costs the user's afternoon.
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(250))
        .timeout(std::time::Duration::from_millis(1_000))
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

/// Show the handoff waiting for the next session, without consuming it.
///
/// Deliberately a peek, not a claim: running this to see what is waiting must
/// not be the reason the next agent session starts with nothing.
fn cmd_handoff(workstream: Option<String>, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;

    let workstream_id = match workstream.as_deref().map(str::trim) {
        Some(slug) => Some(
            store
                .find_workstream(scope.project_id, slug)?
                .ok_or_else(|| anyhow::anyhow!("no workstream named {slug:?}"))?
                .id,
        ),
        None => None,
    };

    match store.peek_handoff(scope.project_id, workstream_id)? {
        Some(body) => {
            match &workstream {
                Some(slug) => println!("📋 Pending handoff for workstream {slug}:"),
                None => println!("📋 Pending handoff:"),
            }
            println!();
            println!("{body}");
        }
        None => println!("Nothing waiting — the last session left no handoff, or it was already claimed."),
    }
    Ok(())
}

fn cmd_sessions(limit: Option<usize>, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;
    let sessions = store.recent_sessions(scope.project_id, limit.unwrap_or(20))?;

    if sessions.is_empty() {
        println!("No sessions recorded for {}.", scope.scope);
        return Ok(());
    }

    for session in &sessions {
        let short: String = session.id.to_string().chars().take(8).collect();
        let when = session.started_at.to_string();
        let when = when.split('.').next().unwrap_or(&when);
        let workstream = match &session.workstream {
            Some(slug) => format!(" · {slug}"),
            None => String::new(),
        };
        println!(
            "{short}  {when}  {:<12} {:<7} {} obs{workstream}",
            session.agent, session.state, session.observation_count
        );
    }
    Ok(())
}

fn cmd_show_page(path: &str, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, data, _store) = open_project(data_dir)?;
    let wiki = Wiki::open(data.wiki())?;
    let page_path = anamnesis_core::page::PagePath::parse(path)?;

    if !wiki.exists(&scope.scope, &page_path) {
        anyhow::bail!(
            "no page at {page_path} — looked in {}",
            wiki.locate(&scope.scope, &page_path).display()
        );
    }

    let page = wiki.read_page(&scope.scope, &page_path)?;
    let fm = &page.frontmatter;

    println!("📄 {}", fm.title);
    println!("   {page_path}");
    println!("   {} · {}", fm.tier.as_str(), fm.status.as_str());
    if fm.pinned {
        println!("   pinned");
    }
    if fm.canonical {
        println!("   canonical");
    }
    if let Some(expires) = fm.expires_at {
        println!("   expires {expires}");
    }
    if !fm.entities.is_empty() {
        let names: Vec<&str> = fm.entities.iter().map(|e| e.as_str()).collect();
        println!("   entities: {}", names.join(", "));
    }
    println!();
    println!("{}", page.body.trim_end());
    Ok(())
}
