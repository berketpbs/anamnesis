//! CLI entry point for anamnesis.

mod bootstrap;
mod reindex;
mod sweep;

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
#[command(
    long_about = "Anamnesis preserves context across AI agent sessions through a persistent wiki.
Quit Claude Code mid-task, start Codex in the same directory, and the next agent
receives a bounded handoff with previous decisions, attempted approaches, and open questions."
)]
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
    ///
    /// Answers the question someone actually has: is my work being recorded
    /// right now? That takes three facts, not one — whether the server is
    /// reachable, when capture last reached the index, and what is in memory.
    Status {
        /// Show detailed information
        #[arg(short, long)]
        verbose: bool,

        /// Server whose reachability to report
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,
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
        #[arg(
            long,
            env = "ANAMNESIS_SERVER",
            default_value = "http://127.0.0.1:8080"
        )]
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

    /// Rebuild the index from the wiki and the raw transcripts
    ///
    /// Safe to run at any time: every identifier is derived, so a rebuild
    /// reproduces the same rows rather than duplicating them.
    Reindex,

    /// Seed an empty memory from the repository's git history
    ///
    /// Writes what the commits already say - who works here, where the churn
    /// is, what just landed - as `bootstrap/` pages. Existing pages are left
    /// alone unless `--force` is given.
    Bootstrap {
        /// Repository path
        #[arg(long, default_value = ".")]
        repo: PathBuf,

        /// Rewrite bootstrap pages that already exist
        #[arg(long)]
        force: bool,

        /// Stop walking after this many commits
        #[arg(long, default_value_t = bootstrap::DEFAULT_MAX_COMMITS)]
        max_commits: usize,

        /// Print what would be written without writing it
        #[arg(long)]
        dry_run: bool,
    },

    /// Forget pages that have decayed past the point of being worth keeping
    ///
    /// Reports and changes nothing unless `--apply` is given. Pinned, durable,
    /// canonical, and known-wrong pages are never swept; a page whose
    /// `expires_at` has passed goes whatever its score. Deleted pages remain
    /// in the wiki's git history.
    Sweep {
        /// Actually forget the pages, instead of only reporting them
        #[arg(long)]
        apply: bool,

        /// Retention score below which a page is forgotten
        ///
        /// Overrides `[decay] threshold` in the marker file.
        #[arg(long)]
        threshold: Option<f64>,

        /// Show every page judged, not only the ones that would go
        #[arg(short, long)]
        verbose: bool,
    },

    /// Notice what a project's memory could do better, and act on it
    ///
    /// Files proposals from signals the system already records: a page several
    /// sessions kept coming back to should be durable, and a page several
    /// pages link to should exist. Nothing is changed unless the project has
    /// set `[auto_improve] require_approval = false`.
    Improve {
        /// Carry out one proposal, named by any unambiguous prefix of its id
        #[arg(long, value_name = "ID")]
        apply: Option<String>,

        /// Never propose this again, named by any unambiguous prefix of its id
        #[arg(long, value_name = "ID")]
        dismiss: Option<String>,

        /// Show proposals that were already decided, as well as open ones
        #[arg(long)]
        history: bool,
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
        Commands::Status { verbose, server } => {
            cmd_status(verbose, &server, cli.data_dir.clone())?;
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
            cmd_write_page(
                &path,
                &title,
                &body,
                pinned,
                expires_at,
                cli.data_dir.clone(),
            )?;
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
        Commands::Reindex => {
            cmd_reindex(cli.data_dir.clone())?;
        }
        Commands::Bootstrap {
            repo,
            force,
            max_commits,
            dry_run,
        } => {
            cmd_bootstrap(&repo, force, max_commits, dry_run, cli.data_dir.clone())?;
        }
        Commands::Sweep {
            apply,
            threshold,
            verbose,
        } => {
            cmd_sweep(apply, threshold, verbose, cli.data_dir.clone())?;
        }
        Commands::Improve {
            apply,
            dismiss,
            history,
        } => {
            cmd_improve(apply, dismiss, history, cli.data_dir.clone())?;
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

fn cmd_status(verbose: bool, server: &str, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
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
        println!(
            "  Wiki:         {}",
            data.wiki_scope(&scope.scope).display()
        );
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

    if !data.db_file().exists() {
        println!();
        println!("  Not initialized yet — run `anamnesis init`.");
        return Ok(());
    }

    // Migrated like every other read-only command opens it. A status that read
    // an older schema would be describing a different database from the one
    // `search` and `sessions` use, which is worse than taking a moment here.
    let store = Store::open(data.db_file())?;
    store.migrate()?;

    let now = Timestamp::now();
    println!();
    println!(
        "  Server:    {}",
        describe_server(server, &probe_server(server))
    );
    println!(
        "  Capture:   {}",
        describe_capture(store.last_observation_at(scope.project_id)?, now)
    );
    println!(
        "  Memory:    {}",
        describe_memory(
            store.session_count(scope.project_id)?,
            store.page_count(scope.project_id)?,
            store.peek_handoff(scope.project_id, None)?.is_some(),
        )
    );

    Ok(())
}

/// Whether the process that records events is answering.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerState {
    /// Reachable, and answering as anamnesis does.
    Running,
    /// Something is listening, but it is not answering `/health`.
    Foreign(u16),
    /// Nothing answered.
    Down,
}

/// Ask the server whether it is there.
///
/// The budget is deliberately more generous than the hook's one second. A hook
/// that waits is a stutter in someone's editing session, so it gives up fast;
/// here a person has asked and is watching the terminal, and calling a
/// slow-but-running server dead is the more expensive mistake of the two.
fn probe_server(server: &str) -> ServerState {
    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return ServerState::Down;
    };

    match client.get(format!("{server}/health")).send() {
        Ok(response) if response.status().is_success() => ServerState::Running,
        Ok(response) => ServerState::Foreign(response.status().as_u16()),
        Err(_) => ServerState::Down,
    }
}

/// One line for where events are being delivered, and whether anyone is there.
///
/// The address is always named. Someone running the server on another port has
/// not lost their memory, and a line that said only "unreachable" would send
/// them looking for the wrong problem.
fn describe_server(server: &str, state: &ServerState) -> String {
    match state {
        ServerState::Running => format!("running at {server}"),
        ServerState::Foreign(code) => {
            format!("something else answered {code} at {server}")
        }
        ServerState::Down => {
            format!("not running at {server} — nothing is being captured")
        }
    }
}

/// One line for when capture last reached the index.
///
/// A reachable server proves nothing on its own: it records only what a
/// harness sends it, and hooks that were never installed look exactly like a
/// quiet afternoon. This is the half of the answer that comes from evidence.
fn describe_capture(last: Option<Timestamp>, now: Timestamp) -> String {
    match last {
        Some(at) => format!("last event {}", describe_age(at, now)),
        None => "nothing captured yet — run `anamnesis install-hooks`".to_owned(),
    }
}

/// One line for what this project's memory currently holds.
fn describe_memory(sessions: i64, pages: i64, handoff: bool) -> String {
    format!(
        "{} · {} · {}",
        plural(sessions, "session"),
        plural(pages, "page"),
        if handoff {
            "handoff waiting"
        } else {
            "no handoff waiting"
        }
    )
}

/// `1 page` / `2 pages`, so a count never has to be read as `1 page(s)`.
fn plural(count: i64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Explain, in one line, how the project was identified.
///
/// A wrong scope is the likeliest reason memory appears empty, so this is the
/// first thing `status` should make visible.
fn describe_source(source: &ScopeSource) -> String {
    match source {
        ScopeSource::Marker { path, legacy } => {
            let name = if *legacy { "legacy marker" } else { "marker" };
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
fn open_project(
    data_dir: Option<PathBuf>,
) -> anyhow::Result<(anamnesis_core::scope::ResolvedScope, DataDir, Store)> {
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
    let query_vector = embedder
        .as_ref()
        .and_then(|embedder| match embedder.embed(query) {
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
        frontmatter.expires_at = Some(stamp.parse().map_err(|_| {
            anyhow::anyhow!("--expires-at {expires:?} is not a date or RFC 3339 timestamp")
        })?);
    }

    let now = Timestamp::now();
    store.upsert_project(&scope, now)?;

    let mut page =
        anamnesis_core::page::Page::new(scope.project_id, page_path.clone(), frontmatter, body);
    let commit = wiki.write_page(&scope.scope, &page, &format!("cli: write {page_path}"))?;
    page.git_commit = Some(commit.clone());

    store.upsert_page(&page, now)?;
    store.set_page_links(
        scope.project_id,
        page.id,
        &anamnesis_wiki::extract_links(body),
    )?;

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
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
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
    let raw = anamnesis_store::RawSpool::new(data.raw());

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
    println!(
        "   auto-improve: every {}s, for projects whose marker asks for it",
        anamnesis_web::improve::TICK.as_secs()
    );
    println!("   transcripts: {}", raw.root().display());
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
        anamnesis_web::AppState::new(store, wiki)
            .with_raw(Some(raw))
            .with_llm(settings),
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
            eprintln!(
                "anamnesis: server rejected event ({status}): {}",
                detail.trim()
            );
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
            .query(&[("agent", agent), ("session_id", &session_id), ("cwd", &cwd)])
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

fn cmd_reindex(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;
    data.ensure_layout()?;

    // Opening creates the database when it is missing, which is the case
    // this command exists for.
    let store = Store::open(data.db_file())?;
    store.migrate()?;
    let wiki = Wiki::open(data.wiki())?;
    let raw = anamnesis_store::RawSpool::new(data.raw());

    println!("♻️  Rebuilding the index for {}", scope.scope);
    println!(
        "   wiki:        {}",
        data.wiki_scope(&scope.scope).display()
    );
    println!("   transcripts: {}", raw.root().display());
    println!();

    let report = reindex::rebuild(&store, &wiki, &raw, &scope, Timestamp::now())?;

    println!("  {} page(s) indexed", report.pages);
    println!(
        "  {} session(s), {} observation(s) recovered",
        report.sessions, report.observations
    );
    if report.orphaned_files > 0 {
        println!(
            "  {} transcript file(s) had no session header and were skipped",
            report.orphaned_files
        );
    }
    println!();
    println!("  Pending handoffs are not restored: a handoff says what the *next*");
    println!("  session should know, and reviving a stale one is worse than none.");

    Ok(())
}

/// Seed the wiki from the repository's git history.
///
/// The scope is resolved from the repository being surveyed rather than the
/// current directory: `anamnesis bootstrap --repo ../other-project` must seed
/// that project's memory, not this one's.
fn cmd_bootstrap(
    repo: &std::path::Path,
    force: bool,
    max_commits: usize,
    dry_run: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let scope = resolve_scope(&repo)?;
    let data = DataDir::resolve(data_dir)?;

    println!("📖 Bootstrapping {} from git history", scope.scope);
    println!("   repo: {}", display_path(&repo));
    println!();

    let survey = bootstrap::survey(&repo, max_commits)?;
    if survey.is_empty() {
        println!("  No commits yet - nothing to seed.");
        println!("  Memory fills itself in from sessions as work happens.");
        return Ok(());
    }

    let now = Timestamp::now();
    let drafts = bootstrap::draft(&survey, now)?;

    let bound = if survey.truncated {
        format!(" (stopped at --max-commits {max_commits})")
    } else {
        String::new()
    };
    println!(
        "  {} commit(s) walked{bound}, {} contributor(s), {} hotspot(s) listed",
        survey.commits,
        survey.authors.len(),
        survey.hotspots.len()
    );
    println!();

    if dry_run {
        for item in &drafts {
            let state = if wiki_has(&data, &scope, &item.path) {
                if force { "overwrite" } else { "skip (exists)" }
            } else {
                "write"
            };
            println!("  {state:<14} {}", item.path);
        }
        println!();
        println!("  Dry run - nothing written.");
        return Ok(());
    }

    data.ensure_layout()?;
    let store = Store::open(data.db_file())?;
    store.migrate()?;
    let wiki = Wiki::open(data.wiki())?;

    let report = bootstrap::seed(&store, &wiki, &scope, &drafts, force, now)?;

    for path in &report.written {
        println!("  wrote   {path}");
    }
    for path in &report.skipped {
        println!("  skipped {path} (already exists)");
    }
    println!();
    if !report.skipped.is_empty() {
        println!("  Skipped pages were left alone: bootstrap seeds a memory, it does not");
        println!("  maintain one. Pass --force to rewrite them from git.");
    }
    if !report.written.is_empty() {
        println!("  These pages are derived from commits, not decided by anyone - they rank");
        println!("  below what a session actually learned.");
    }

    Ok(())
}

/// A path as someone would type it, rather than as Windows canonicalizes it.
///
/// `canonicalize` returns the verbatim form — `\\?\C:\repo` — which is correct
/// and unreadable. Only the printed form is trimmed; the path actually used
/// keeps the prefix.
fn display_path(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match shown.strip_prefix(r"\\?\") {
        Some(trimmed) => trimmed.to_owned(),
        None => shown,
    }
}

/// Whether a page already exists, for the dry run's benefit.
///
/// Reads the filesystem rather than opening the wiki: a dry run must not
/// create a git repository as a side effect of being asked what it would do.
fn wiki_has(
    data: &DataDir,
    scope: &anamnesis_core::scope::ResolvedScope,
    path: &anamnesis_core::page::PagePath,
) -> bool {
    data.wiki_scope(&scope.scope).join(path.as_str()).is_file()
}

/// Run an improvement pass, and show what is waiting on a person.
///
/// The same pass the server runs on a schedule, on demand. Which is the point
/// of it being a command as well: `[auto_improve.scheduler]` is off by
/// default, so for most projects this is the only thing that ever looks.
fn cmd_improve(
    apply: Option<String>,
    dismiss: Option<String>,
    history: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let now = Timestamp::now();

    if let Some(prefix) = apply {
        let wiki = Wiki::open(data.wiki())?;
        return decide_by_applying(&store, &wiki, &scope, &prefix, now);
    }
    if let Some(prefix) = dismiss {
        return decide_by_dismissing(&store, &scope, &prefix, now);
    }

    println!("🌱 Improving {}", scope.scope);
    println!();

    let wiki = Wiki::open(data.wiki())?;
    let report = anamnesis_web::improve::run_pass(
        &store,
        &wiki,
        &scope.scope,
        scope.project_id,
        &scope.auto_improve,
        now,
    )?;

    let Some(report) = report else {
        println!("  Auto-improve is off for this project.");
        println!(
            "  Set `[auto_improve] enabled = true` in {} to turn it on.",
            marker_name(&scope)
        );
        return Ok(());
    };
    store.mark_improved(scope.project_id, now)?;

    println!(
        "  {} noticed, {} refreshed, {} resolved.",
        report.filed.filed, report.filed.refreshed, report.filed.resolved
    );

    for carried in &report.carried {
        let done = match &carried.outcome {
            anamnesis_web::improve::Outcome::Promoted { commit } => {
                format!("promoted (commit {})", &commit[..commit.len().min(8)])
            }
            anamnesis_web::improve::Outcome::AlreadyDurable => {
                "already promoted by someone else".to_owned()
            }
            anamnesis_web::improve::Outcome::NeedsAPerson => "needs a person".to_owned(),
        };
        println!("  ✓ {} — {done}", carried.subject);
    }
    for (subject, error) in &report.failures {
        println!("  ⚠ {subject} — {error}");
    }

    print_proposals(&store, &scope, history, now)?;

    if report.open > 0 && scope.auto_improve.require_approval {
        println!();
        println!("  This project requires approval, so nothing was changed.");
        println!("  anamnesis improve --apply <id>    carry one out");
        println!("  anamnesis improve --dismiss <id>  never propose it again");
    }

    Ok(())
}

/// Print the proposals a project is sitting on.
fn print_proposals(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    history: bool,
    now: Timestamp,
) -> anyhow::Result<()> {
    let proposals = store.proposals(scope.project_id, !history)?;
    if proposals.is_empty() {
        println!();
        println!("  Nothing to propose. The memory is in good shape.");
        return Ok(());
    }

    let mut open_shown = false;
    let mut decided_shown = false;
    for proposal in &proposals {
        if proposal.state.is_open() && !open_shown {
            println!();
            println!("  Open:");
            open_shown = true;
        }
        if !proposal.state.is_open() && !decided_shown {
            println!();
            println!("  Decided:");
            decided_shown = true;
        }

        let short = proposal.id.to_string();
        let age = describe_age(proposal.created_at, now);
        println!(
            "    {}  {:<28}  {}",
            &short[..8],
            proposal.kind.action(),
            proposal.subject
        );
        println!("              {}", proposal.rationale);
        if proposal.state.is_open() {
            println!("              noticed {age}");
        } else {
            println!("              {} {age}", proposal.state.as_str());
        }
    }
    Ok(())
}

/// Carry out one proposal, named by any unambiguous prefix of its id.
fn decide_by_applying(
    store: &Store,
    wiki: &Wiki,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
    now: Timestamp,
) -> anyhow::Result<()> {
    let proposal = one_proposal(store, scope, prefix)?;
    anyhow::ensure!(
        proposal.state.is_open(),
        "that proposal was already {} — a decision is made once",
        proposal.state.as_str()
    );

    match anamnesis_web::improve::apply(
        store,
        wiki,
        &scope.scope,
        scope.project_id,
        &proposal,
        now,
    )? {
        anamnesis_web::improve::Outcome::Promoted { commit } => {
            println!("✓ Promoted {} to the semantic tier", proposal.subject);
            println!("  commit {}", &commit[..commit.len().min(8)]);
            println!();
            println!("  It is now exempt from the decay sweep.");
        }
        anamnesis_web::improve::Outcome::AlreadyDurable => {
            println!("· {} was already promoted", proposal.subject);
            println!("  The proposal is resolved; nothing was written.");
        }
        anamnesis_web::improve::Outcome::NeedsAPerson => {
            println!("· Nothing here can be done mechanically.");
            println!("  {}: {}", proposal.kind.action(), proposal.subject);
            println!("  {}", proposal.rationale);
            println!();
            println!("  Left open. Write it with `anamnesis write-page`, or dismiss it.");
        }
    }
    Ok(())
}

/// Dismiss one proposal, so no later pass files it again.
fn decide_by_dismissing(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
    now: Timestamp,
) -> anyhow::Result<()> {
    let proposal = one_proposal(store, scope, prefix)?;
    anyhow::ensure!(
        store.decide_proposal(
            proposal.id,
            anamnesis_core::improve::ProposalState::Dismissed,
            now
        )?,
        "that proposal was already {} — a decision is made once",
        proposal.state.as_str()
    );

    println!("· Dismissed: {}", proposal.subject);
    println!("  Later passes will notice the same thing and leave it alone.");
    Ok(())
}

/// Resolve an id prefix to exactly one proposal.
///
/// Refuses an ambiguous prefix rather than acting on whichever row sorted
/// first: these decisions are permanent, and "it picked the other one" is not
/// a mistake anyone can undo.
fn one_proposal(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
) -> anyhow::Result<anamnesis_store::StoredProposal> {
    let mut matches = store.proposals_matching(scope.project_id, prefix)?;
    match matches.len() {
        0 => anyhow::bail!("no proposal in {} starts with {prefix:?}", scope.scope),
        1 => Ok(matches.remove(0)),
        _ => {
            let listed: Vec<String> = matches
                .iter()
                .map(|proposal| format!("{} ({})", &proposal.id.to_string()[..8], proposal.subject))
                .collect();
            anyhow::bail!(
                "{prefix:?} matches {}: {}",
                matches.len(),
                listed.join(", ")
            )
        }
    }
}

/// How long ago something happened, in the roughest useful unit.
fn describe_age(then: Timestamp, now: Timestamp) -> String {
    let minutes = (now.as_millisecond() - then.as_millisecond()) / 60_000;
    match minutes {
        ..1 => "just now".to_owned(),
        1..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

/// The marker file to point someone at, by the name it actually has.
fn marker_name(scope: &anamnesis_core::scope::ResolvedScope) -> String {
    match &scope.marker {
        Some(path) => path.display().to_string(),
        None => ".anamnesis.toml".to_owned(),
    }
}

/// Report what a project's memory would forget, and forget it when asked.
///
/// Reporting is the default because the threshold is a guess until someone
/// has seen it applied to a real wiki, and because the alternative — a
/// command that deletes pages the first time it is run out of curiosity — is
/// not a memory system anyone should trust.
fn cmd_sweep(
    apply: bool,
    threshold: Option<f64>,
    verbose: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;

    let mut policy = scope.decay.policy()?;
    if let Some(threshold) = threshold {
        anyhow::ensure!(
            threshold.is_finite() && threshold >= 0.0,
            "--threshold {threshold} is not a finite number of zero or more"
        );
        policy.threshold = threshold;
    }

    let now = Timestamp::now();
    let plan = sweep::plan(&store, scope.project_id, policy, now)?;

    println!("🧹 Sweeping {}", scope.scope);
    println!("   threshold {:.3}", policy.threshold);
    println!();

    if plan.scanned() == 0 {
        // A sweep judges what the index knows, so an index that knows nothing
        // is indistinguishable from a project with no memory — and the two
        // want opposite things done about them.
        println!("  No pages indexed for this project — nothing to sweep.");
        println!("  If its wiki does hold pages, run `anamnesis reindex` first.");
        return Ok(());
    }

    if plan.forget.is_empty() {
        println!("  Nothing has decayed past the threshold.");
    } else {
        let verb = if apply { "Forgetting" } else { "Would forget" };
        println!(
            "  {verb} {} of {} page(s):",
            plan.forget.len(),
            plan.scanned()
        );
        print_judged(&plan.forget, now);
    }

    println!();
    println!(
        "  {} kept, {} exempt{}",
        plan.keep.len(),
        plan.exempt.len(),
        describe_exemptions(&plan)
    );

    if verbose {
        if !plan.keep.is_empty() {
            println!();
            println!("  Kept:");
            print_judged(&plan.keep, now);
        }
        if !plan.exempt.is_empty() {
            println!();
            println!("  Exempt:");
            print_judged(&plan.exempt, now);
        }
    }

    // A contradiction between two things the same author wrote. Reported
    // whether or not anything was swept, because the pin is the only reason
    // the page is still here.
    for judged in plan.conflicts() {
        let deadline = judged
            .row
            .facts
            .expires_at
            .map(|at| at.strftime("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "an earlier date".to_owned());
        println!();
        println!(
            "  ⚠ {} expired {deadline} but is exempt ({}) — the exemption won.",
            judged.row.path,
            judged.reason(now)
        );
    }

    if !apply {
        if !plan.forget.is_empty() {
            println!();
            println!("  Nothing was deleted. Re-run with --apply to forget these pages.");
        }
        return Ok(());
    }

    if plan.forget.is_empty() {
        return Ok(());
    }

    let wiki = Wiki::open(data.wiki())?;
    let swept = sweep::apply(&store, &wiki, &scope, &plan, policy, now)?;

    println!();
    println!(
        "  {} page(s) forgotten, {} index row(s) dropped",
        swept.pages, swept.rows
    );
    match &swept.commit {
        Some(commit) => println!("  commit {}", &commit[..commit.len().min(8)]),
        None => println!("  the wiki had nothing to record"),
    }
    println!();
    println!("  Every page removed is still in the wiki's git history:");
    println!("  git -C {} show HEAD", wiki.root().display());

    Ok(())
}

/// Print one judged page per line, path first, reason aligned after it.
fn print_judged(judged: &[sweep::Judged], now: Timestamp) {
    let width = judged
        .iter()
        .map(|item| item.row.path.as_str().len())
        .max()
        .unwrap_or(0)
        .min(60);
    for item in judged {
        println!(
            "    {:width$}  {}",
            item.row.path.as_str(),
            item.reason(now)
        );
    }
}

/// Spell out which rules spared pages, for the summary line.
fn describe_exemptions(plan: &sweep::Plan) -> String {
    let counts = plan.exemptions();
    if counts.is_empty() {
        return ".".to_owned();
    }
    let described: Vec<String> = counts
        .iter()
        .map(|(exemption, count)| format!("{count} {}", exemption.as_str()))
        .collect();
    format!(" ({}).", described.join(", "))
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
        None => println!(
            "Nothing waiting — the last session left no handoff, or it was already claimed."
        ),
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
    let (scope, data, store) = open_project(data_dir)?;
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
    if let Some(replaces) = &fm.supersedes {
        println!("   replaces {replaces}");
    }
    // Worth saying loudly: retrieval stopped offering this page the moment
    // something replaced it, so anyone reading it here found it by name and
    // has no other way to learn that.
    if let Some(replacement) = store.superseded_by(scope.project_id, &page_path)? {
        println!("   ⚠ replaced by {replacement}");
    }
    if !fm.entities.is_empty() {
        let names: Vec<&str> = fm.entities.iter().map(|e| e.as_str()).collect();
        println!("   entities: {}", names.join(", "));
    }
    println!();
    println!("{}", page.body.trim_end());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    /// The whole point of the line: an unreachable server means events are
    /// being dropped right now, and saying so is the only warning anyone gets.
    #[test]
    fn a_server_that_is_not_running_says_capture_has_stopped() {
        let line = describe_server("http://127.0.0.1:8080", &ServerState::Down);
        assert!(line.contains("nothing is being captured"), "{line}");
        assert!(line.contains("http://127.0.0.1:8080"), "{line}");
    }

    /// Naming the address is what separates "your memory is gone" from "you
    /// are looking at the wrong port".
    #[test]
    fn every_server_line_names_where_it_looked() {
        for state in [
            ServerState::Running,
            ServerState::Foreign(404),
            ServerState::Down,
        ] {
            let line = describe_server("http://example:9999", &state);
            assert!(line.contains("http://example:9999"), "{state:?}: {line}");
        }
    }

    /// A port answering something that is not anamnesis is a different problem
    /// from a port answering nothing, and needs a different fix.
    #[test]
    fn a_foreign_listener_is_not_reported_as_a_running_server() {
        let line = describe_server("http://127.0.0.1:8080", &ServerState::Foreign(404));
        assert!(!line.contains("running at"), "{line}");
        assert!(line.contains("404"), "{line}");
    }

    /// Hooks that were never installed look exactly like a quiet afternoon, so
    /// the empty case has to point at the thing that fixes it.
    #[test]
    fn capturing_nothing_yet_points_at_install_hooks() {
        let line = describe_capture(None, at("2026-08-25T12:00:00Z"));
        assert!(line.contains("install-hooks"), "{line}");
    }

    #[test]
    fn capture_reports_how_long_ago_the_last_event_landed() {
        let line = describe_capture(Some(at("2026-08-25T11:57:00Z")), at("2026-08-25T12:00:00Z"));
        assert_eq!(line, "last event 3m ago");
    }

    #[test]
    fn a_waiting_handoff_is_reported_as_waiting() {
        assert!(describe_memory(2, 1, true).contains("handoff waiting"));
        assert!(describe_memory(2, 1, false).contains("no handoff waiting"));
    }

    #[test]
    fn counts_are_pluralised_rather_than_parenthesised() {
        assert_eq!(plural(0, "page"), "0 pages");
        assert_eq!(plural(1, "page"), "1 page");
        assert_eq!(plural(2, "page"), "2 pages");
    }
}
