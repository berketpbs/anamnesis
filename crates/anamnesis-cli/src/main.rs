//! CLI entry point for anamnesis.

mod bootstrap;
mod hooks;
mod mcp_config;
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

        /// Token to present, when the server requires one
        #[arg(long, env = anamnesis_web::auth::TOKEN_ENV, hide_env_values = true)]
        token: Option<String>,
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

        /// Temporal tier: working, episodic, semantic, or procedural
        ///
        /// `semantic` and `procedural` are durable — the decay sweep does not
        /// reach them. Defaults to `episodic`, which does decay.
        #[arg(long)]
        tier: Option<String>,

        /// Trust level: active, historical, do-not-answer-from, or superseded
        #[arg(long)]
        status: Option<String>,

        /// Declare this page authoritative on its subject
        #[arg(long)]
        canonical: bool,

        /// Canonical names this page is about, repeated or comma-separated
        ///
        /// The entity retrieval stream matches on these, so a page that
        /// declares none is reachable through its words alone.
        #[arg(long, value_delimiter = ',')]
        entity: Vec<String>,

        /// Path of the page this one replaces
        ///
        /// Recorded rather than deleting the old page: retrieval stops
        /// offering it, and a later reader can still see what was believed
        /// before and why it changed.
        #[arg(long)]
        supersedes: Option<String>,

        /// Write into the workspace's shared scope instead of this project
        ///
        /// Every project in the workspace searches it, so this is where a
        /// policy goes — something true of all of them rather than of the one
        /// you happen to be standing in.
        #[arg(long)]
        global: bool,
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

        /// Do not watch the wiki for edits made outside anamnesis
        ///
        /// Without the watcher, a page edited or deleted by hand reaches the
        /// index only when `anamnesis reindex` is run.
        #[arg(long)]
        no_watch: bool,

        /// Serve an address other than localhost with no token configured
        ///
        /// Without a token, everything this server holds — every prompt, every
        /// file path, every summary — is readable by anything that can reach
        /// the port. On loopback that is the machine's own boundary; off it,
        /// it is the network's. Say so deliberately, or set ANAMNESIS_TOKEN.
        #[arg(long)]
        allow_anonymous: bool,
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

        /// Token to present, when the server requires one
        ///
        /// Read from the environment so the secret never has to be written
        /// into a settings file that the harness reads aloud.
        #[arg(long, env = anamnesis_web::auth::TOKEN_ENV, hide_env_values = true)]
        token: Option<String>,
    },

    /// Wire the agent's lifecycle hooks to anamnesis
    ///
    /// Prints the configuration by default. `--write` merges it into the
    /// settings file instead, which is the same thing with the paste-it-
    /// yourself mistakes removed.
    InstallHooks {
        /// Which harness to configure
        #[arg(long, default_value = "claude-code")]
        agent: String,

        /// Server the hooks should deliver to
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,

        /// Merge the configuration into the settings file instead of printing
        #[arg(long)]
        write: bool,

        /// Settings file to write to, when `--write` is given
        ///
        /// Defaults to this project's `.claude/settings.local.json`. Point it
        /// at the user-level settings to capture every project.
        #[arg(long)]
        settings: Option<PathBuf>,
    },

    /// Mint a token for a server that should not be open to everyone
    ///
    /// Prints a fresh secret and the two variables that use it. Nothing is
    /// stored: the token exists only where it is pasted, which is the one
    /// property a secret has to have.
    Token {
        /// Operator the token belongs to, for a server shared by several people
        #[arg(long)]
        operator: Option<String>,
    },

    /// Score retrieval against a checked-in corpus and its questions
    ///
    /// Answers the question the test suite cannot: not "is this correct" but
    /// "does memory find the page that answers this". Runs against a
    /// throwaway corpus, never your own memory — every query would otherwise
    /// count as a read, and the decay sweep believes those.
    Eval {
        /// Suite file to run. Omitted runs the ones built into this binary.
        #[arg(long)]
        suite: Option<PathBuf>,

        /// Print every case with the rank its answer came back at
        #[arg(short, long)]
        verbose: bool,

        /// Score each retrieval stream on its own, and say what only it finds
        ///
        /// The fused ranking says how good retrieval is; this says which
        /// signal is doing the work, and what deleting one would cost.
        #[arg(long)]
        streams: bool,

        /// Score the same questions once per candidate setting, and rank them
        ///
        /// The knobs fusion is built from were chosen by argument. This is
        /// what replaces the argument with a table — and with more than one
        /// suite loaded, with a table that says whether a setting suits
        /// retrieval or merely suits one corpus.
        #[arg(long)]
        sweep: bool,

        /// Exit non-zero when a suite scores below its own thresholds
        #[arg(long)]
        check: bool,
    },

    /// Register the MCP server with a harness, so an agent can read memory
    ///
    /// The other half of connecting an agent. Hooks record what happens; this
    /// is what lets the agent search what was recorded, rather than being
    /// handed one summary at startup and nothing else.
    InstallMcp {
        /// Which harness to configure
        #[arg(long, default_value = "claude-code")]
        agent: String,

        /// Merge the registration into the configuration file instead of
        /// printing it
        #[arg(long)]
        write: bool,

        /// Configuration file to write to, when `--write` is given
        ///
        /// Defaults to this project's `.mcp.json`.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Repository the server should resolve its scope from
        ///
        /// Defaults to the current directory. Passed explicitly rather than
        /// left to the subprocess's working directory, which the harness
        /// chooses.
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Remove a page on purpose, from the wiki and the index
    ///
    /// The counterpart to `sweep`, which forgets what decayed. This forgets
    /// what was wrong: a page written from a bad reply, a note that turned out
    /// to be untrue, a duplicate. The wiki is a git repository, so what is
    /// removed stays recoverable from its history.
    Forget {
        /// Page paths, e.g. `sessions/2026-08-29-3da85483.md`
        #[arg(required = true)]
        paths: Vec<String>,
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

        /// Operator whose slot to look in, where `[slots] per_user` is on
        ///
        /// Names a slot; it proves nothing about who is asking. Over HTTP a
        /// bearer token settles that — here the index is already open in front
        /// of whoever ran the command.
        #[arg(long)]
        operator: Option<String>,

        /// Throw the waiting handoff away instead of showing it
        ///
        /// For a note that is wrong: written from a bad model reply, or about
        /// work that was abandoned. Claiming it to be rid of it would put it
        /// in a session's context, which is the thing being avoided.
        #[arg(long)]
        discard: bool,
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
    //
    // The server also writes to a file, because it is the one command nobody
    // is watching. It runs for days in a terminal that gets closed, and when
    // it stops, stderr stops with it: this repository's own memory went four
    // days without recording anything, and afterwards there was no way to say
    // when the server had died or why. `logs/` has been in the data-directory
    // layout since the first commit, documented as "rolling trace output",
    // with nothing ever written to it.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter(cli.debug)));
    let log_file = matches!(cli.command, Commands::Serve { .. })
        .then(|| open_log_file(cli.data_dir.clone()))
        .flatten();

    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
        match log_file {
            // Written straight through rather than through a background
            // buffer: what a buffer loses is the last few lines before a
            // crash, which are the ones this file exists to keep.
            Some(file) => registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(file),
                )
                .init(),
            None => registry.init(),
        }
    }

    match cli.command {
        Commands::Status {
            verbose,
            server,
            token,
        } => {
            cmd_status(verbose, &server, token.as_deref(), cli.data_dir.clone())?;
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
            tier,
            status,
            canonical,
            entity,
            supersedes,
            global,
        } => {
            cmd_write_page(
                &path,
                &title,
                &body,
                PageOptions {
                    pinned,
                    expires_at,
                    tier,
                    status,
                    canonical,
                    entities: entity,
                    supersedes,
                    global,
                },
                cli.data_dir.clone(),
            )?;
        }
        Commands::Init => {
            cmd_init(cli.data_dir.clone())?;
        }
        Commands::Mcp { repo } => {
            cmd_mcp(&repo, cli.data_dir.clone())?;
        }
        Commands::Serve {
            port,
            bind,
            no_watch,
            allow_anonymous,
        } => {
            cmd_serve(
                &bind,
                port,
                !no_watch,
                allow_anonymous,
                cli.data_dir.clone(),
            )?;
        }
        Commands::Hook {
            agent,
            server,
            token,
        } => {
            cmd_hook(&agent, &server, token.as_deref());
        }
        Commands::InstallHooks {
            agent,
            server,
            write,
            settings,
        } => {
            cmd_install_hooks(&agent, &server, write, settings)?;
        }
        Commands::InstallMcp {
            agent,
            write,
            config,
            repo,
        } => {
            cmd_install_mcp(&agent, write, config, repo)?;
        }
        Commands::Token { operator } => {
            cmd_token(operator.as_deref())?;
        }
        Commands::Eval {
            suite,
            verbose,
            check,
            streams,
            sweep,
        } => {
            cmd_eval(suite.as_deref(), verbose, check, streams, sweep)?;
        }
        Commands::Forget { paths } => {
            cmd_forget(&paths, cli.data_dir.clone())?;
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
        Commands::Handoff {
            workstream,
            operator,
            discard,
        } => {
            cmd_handoff(workstream, operator, discard, cli.data_dir.clone())?;
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

/// The log filter used when `RUST_LOG` says nothing.
///
/// `info` for anamnesis, because consolidation happens after the response has
/// been sent and a model that refuses or times out leaves no trace anywhere
/// else. `warn` for refinery, because it logs the **entire SQL text of every
/// migration** at info — ten migrations of schema, several screens of it, on
/// the first run of every command that opens the index. What that buries is
/// `anamnesis init` saying where the memory now lives, which is the one thing
/// the person running it was reading for, and the SQL it buries it under is
/// checked into this repository.
///
/// `--debug` restores it, and so does an explicit `RUST_LOG`: a migration that
/// fails halfway is exactly when someone wants to see the statement.
/// Open the server's rolling log, or nothing if the data directory cannot be
/// reached.
///
/// Failing to log is never a reason to fail to serve: a data directory that
/// cannot be resolved is about to be reported properly by `serve` itself, and
/// reporting it twice — once from a logging helper that has no subscriber yet
/// — would replace a clear error with a confusing one.
///
/// One file per day, fourteen kept. A log nobody prunes is a data directory
/// that grows without limit, and two weeks is longer than it has ever taken to
/// notice that memory stopped.
fn open_log_file(
    data_dir: Option<PathBuf>,
) -> Option<tracing_appender::rolling::RollingFileAppender> {
    let data = DataDir::resolve(data_dir).ok()?;
    data.ensure_layout().ok()?;
    tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("anamnesis")
        .filename_suffix("log")
        .max_log_files(14)
        .build(data.logs())
        .ok()
}

fn default_filter(debug: bool) -> &'static str {
    if debug {
        "debug"
    } else {
        "info,refinery_core=warn"
    }
}

fn cmd_status(
    verbose: bool,
    server: &str,
    token: Option<&str>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
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
    let reachable = probe_server(server);
    let authenticated = probe_auth(server, token, &reachable);
    println!();
    println!("  Server:    {}", describe_server(server, &reachable));
    println!(
        "  Auth:      {}",
        describe_auth(&authenticated, token.is_some())
    );
    println!(
        "  Capture:   {}",
        describe_capture(store.last_observation_at(scope.project_id)?, now)
    );
    // Where a project keeps a slot per operator, the handoff reported has to
    // be the one *this* machine would be handed. Reporting the shared slot
    // would tell an operator with a note waiting that nothing is waiting.
    let operator = if scope.slots.per_user {
        authenticated.operator()
    } else {
        None
    };
    let slot = anamnesis_core::handoff::Slot::shared().for_operator(operator.clone());
    println!(
        "  Memory:    {}",
        describe_memory(
            store.session_count(scope.project_id)?,
            store.page_count(scope.project_id)?,
            store.peek_handoff(scope.project_id, &slot)?.is_some(),
            operator.as_ref(),
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

/// What the server makes of this machine's token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthState {
    /// The server did not answer the question, so nothing is claimed.
    Unknown,
    /// No token is required. Every caller is accepted.
    Open,
    /// A token was accepted, naming this operator when it named one.
    Accepted(Option<String>),
    /// A token is required and this machine's was not one of them.
    Rejected,
}

impl AuthState {
    /// The operator the server named, when it named one this crate can use.
    ///
    /// A name that will not parse is treated as no name: it can only come from
    /// a server newer than this client, and guessing at it would key a slot to
    /// something that is not an operator.
    fn operator(&self) -> Option<anamnesis_core::scope::OperatorName> {
        match self {
            Self::Accepted(Some(name)) => anamnesis_core::scope::OperatorName::parse(name).ok(),
            _ => None,
        }
    }
}

/// Ask the server what it makes of this machine's token.
///
/// `/whoami` and not `/handoff`, because the question has to be asked without
/// changing anything: collecting a handoff to find out whether a token works
/// would spend the handoff.
fn probe_auth(server: &str, token: Option<&str>, reachable: &ServerState) -> AuthState {
    if *reachable != ServerState::Running {
        return AuthState::Unknown;
    }

    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return AuthState::Unknown;
    };

    let mut request = client.get(format!("{server}/whoami"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    match request.send() {
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            AuthState::Rejected
        }
        Ok(response) if response.status().is_success() => {
            let body = response.json::<serde_json::Value>().unwrap_or_default();
            if body.get("auth").and_then(|v| v.as_str()) == Some("open") {
                AuthState::Open
            } else {
                AuthState::Accepted(
                    body.get("operator")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                )
            }
        }
        // Anything else — including the 404 an older server returns — is a
        // question this server did not answer, and guessing at it would be
        // worse than saying so.
        Ok(_) | Err(_) => AuthState::Unknown,
    }
}

/// One line for whether this memory is protected, and whether this machine
/// gets in.
///
/// Both halves are needed. A server that requires tokens is no comfort if the
/// hooks on this machine are being turned away, and that failure is otherwise
/// invisible: rejected events look exactly like a quiet afternoon.
fn describe_auth(state: &AuthState, presented: bool) -> String {
    let token_env = anamnesis_web::auth::TOKEN_ENV;
    match state {
        AuthState::Unknown => "unknown — the server did not answer".to_owned(),
        AuthState::Open => {
            "not required — anything that can reach this port can read this memory".to_owned()
        }
        AuthState::Accepted(None) => "required — this client's token was accepted".to_owned(),
        AuthState::Accepted(Some(operator)) => {
            format!("required — this client is {operator}")
        }
        AuthState::Rejected if presented => {
            format!("required — this client's token was rejected; check {token_env}")
        }
        AuthState::Rejected => {
            format!("required — this client has no token; set {token_env}")
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
fn describe_memory(
    sessions: i64,
    pages: i64,
    handoff: bool,
    operator: Option<&anamnesis_core::scope::OperatorName>,
) -> String {
    // Named only where a project keeps more than one slot: "no handoff
    // waiting" and "no handoff waiting for alice" are different facts, and on
    // a shared server the second is the one that stops someone concluding
    // their memory is empty.
    let whose = match operator {
        Some(operator) => format!(" for {operator}"),
        None => String::new(),
    };
    format!(
        "{} · {} · {}{whose}",
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
/// The workspace-wide scope this project inherits from.
///
/// One per workspace, derived rather than looked up, so every process that
/// asks for it lands on the same rows. Its root is where its pages live: there
/// is no repository behind it for a relative path to resolve against.
fn global_scope(
    project: &anamnesis_core::scope::ResolvedScope,
    data: &DataDir,
) -> anamnesis_core::scope::ResolvedScope {
    anamnesis_core::scope::ResolvedScope::global(
        &project.scope.workspace,
        data.wiki_global(&project.scope.workspace),
    )
}

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

    // The workspace's shared scope is searched alongside this project's, so a
    // policy written once is found from every project that inherits it.
    let global = global_scope(&scope, &data);
    let hits = store.query_pages_across(
        scope.project_id,
        &[global.project_id],
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

        // Which scope a hit came from is not cosmetic: a policy that applies
        // everywhere and a note about this project are different kinds of
        // answer, and the path alone does not say which is which.
        let from = if hit.project_id == global.project_id {
            format!(" ({})", anamnesis_core::scope::GLOBAL_PROJECT)
        } else {
            String::new()
        };
        println!("{}  {}{}{}", hit.path, hit.title, marks, from);
        println!("    {} · score {:.4}", hit.tier.as_str(), hit.score);
        if !hit.snippet.is_empty() {
            println!("    {}", hit.snippet.replace('\n', " "));
        }
        println!();
    }
    Ok(())
}

/// Everything about a page except what it says.
///
/// A struct rather than eight positional arguments, four of which are `bool`
/// or `Option<String>` and would swap silently.
#[derive(Debug, Default)]
struct PageOptions {
    /// Exempt from the decay sweep.
    pinned: bool,
    /// When the page should be forgotten.
    expires_at: Option<String>,
    /// Temporal tier.
    tier: Option<String>,
    /// Trust level.
    status: Option<String>,
    /// Authoritative on its subject.
    canonical: bool,
    /// Canonical names the page is about.
    entities: Vec<String>,
    /// Page this one replaces.
    supersedes: Option<String>,
    /// Write into the workspace's shared scope rather than this project.
    global: bool,
}

fn cmd_write_page(
    path: &str,
    title: &str,
    body: &str,
    options: PageOptions,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (project, data, store) = open_project(data_dir)?;
    // Resolved from the project either way: the shared scope belongs to the
    // workspace this project is in, so standing somewhere else writes to a
    // different one.
    let scope = if options.global {
        global_scope(&project, &data)
    } else {
        project
    };
    let wiki = Wiki::open(data.wiki())?;

    let page_path = anamnesis_core::page::PagePath::parse(path)?;
    let entities = options
        .entities
        .iter()
        .map(|name| anamnesis_core::page::Entity::parse(name))
        .collect::<anamnesis_core::Result<Vec<_>>>()?;

    let mut frontmatter = anamnesis_core::page::Frontmatter::new(title, entities.clone())?;
    frontmatter.pinned = options.pinned;
    frontmatter.canonical = options.canonical;
    if let Some(tier) = &options.tier {
        frontmatter.tier = anamnesis_core::page::Tier::parse(tier)?;
    }
    if let Some(status) = &options.status {
        frontmatter.status = anamnesis_core::page::PageStatus::parse(status)?;
    }
    if let Some(supersedes) = &options.supersedes {
        frontmatter.supersedes = Some(anamnesis_core::page::PagePath::parse(supersedes)?);
    }
    if let Some(expires) = &options.expires_at {
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

    // Entities as well as links, which this command used to skip because it
    // could not set any. A page whose entities never reach the index is one
    // the entity stream cannot find, however carefully they were declared.
    store.index_page(
        scope.project_id,
        &page,
        &anamnesis_wiki::extract_links(body),
        None,
        now,
    )?;

    if options.global {
        println!("🌍 Wrote {page_path} to {}", scope.scope);
        println!("   every project in {} searches it", scope.scope.workspace);
    } else {
        println!("✍️  Wrote {page_path}");
    }
    println!("   {}", wiki.locate(&scope.scope, &page_path).display());
    println!("   commit {}", &commit[..commit.len().min(8)]);
    println!("   {}", describe_page(&page.frontmatter));
    if let Some(replaced) = &page.frontmatter.supersedes {
        // Said out loud because it is the one flag that changes another page:
        // whatever it named stops being offered to recall.
        println!("   replaces {replaced}, which recall will stop offering");
    }
    Ok(())
}

/// The one-line summary of what a page was written as.
fn describe_page(frontmatter: &anamnesis_core::page::Frontmatter) -> String {
    let mut parts = vec![frontmatter.tier.as_str().to_owned()];
    if frontmatter.status != anamnesis_core::page::PageStatus::default() {
        parts.push(frontmatter.status.as_str().to_owned());
    }
    if frontmatter.canonical {
        parts.push("canonical".to_owned());
    }
    if frontmatter.pinned {
        parts.push("pinned".to_owned());
    }
    if !frontmatter.entities.is_empty() {
        parts.push(format!(
            "entities: {}",
            frontmatter
                .entities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join(" · ")
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

fn cmd_serve(
    bind: &str,
    port: u16,
    watch_wiki: bool,
    allow_anonymous: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let address: std::net::SocketAddr = format!("{bind}:{port}").parse()?;

    // Read before anything is opened: a server whose tokens are misconfigured
    // should not have got as far as touching the data directory, and one that
    // would expose memory to a network should not start at all.
    let auth = anamnesis_web::Auth::from_env()?;
    if let Some(refusal) = refuse_anonymous_exposure(&address, auth.is_open(), allow_anonymous) {
        anyhow::bail!(refusal);
    }

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

    println!("🌐 anamnesis serving on http://{address}");
    println!("   data dir: {}", data.root().display());
    println!("   POST /hook   GET /handoff   GET /whoami   GET /health");
    println!("   auth: {}", describe_serving_auth(&auth));
    println!(
        "   auto-improve: every {}s, for projects whose marker asks for it",
        anamnesis_web::improve::TICK.as_secs()
    );
    println!("   transcripts: {}", raw.root().display());
    println!("   logs:        {}", data.logs().display());
    println!(
        "   wiki edits:  {}",
        if watch_wiki {
            "watched — pages edited by hand are indexed as they are saved"
        } else {
            "not watched — hand edits need `anamnesis reindex`"
        }
    );
    match &settings {
        Some(settings) => println!(
            "   consolidation: {} ({})",
            settings.provider.model(),
            settings.provider.name()
        ),
        None => println!("   consolidation: counted (no model configured)"),
    }

    // The banner above goes to the terminal this was started from, which is
    // exactly the thing that will not exist later. This line goes to the file,
    // so that "when did memory stop" has a first half to compare against.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %address,
        data_dir = %data.root().display(),
        "anamnesis server starting"
    );

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(anamnesis_web::serve(
        address,
        anamnesis_web::AppState::new(store, wiki)
            .with_raw(Some(raw))
            .with_llm(settings)
            .with_auth(auth),
        watch_wiki,
    ))?;
    Ok(())
}

/// Refuse to serve a network address with nothing guarding it.
///
/// The default bind is loopback, where the machine's own boundary is the whole
/// story and a token would only be ceremony — which is why an open server stays
/// legal there, and why every install that predates tokens keeps working. An
/// address reachable from elsewhere is a different proposition: what is behind
/// this port is every prompt someone typed, every path they opened, and every
/// summary written from them. Refusing is recoverable in one command; the
/// alternative failure is silent and permanent.
///
/// `--allow-anonymous` exists because "in front of a proxy that authenticates"
/// is a real deployment, and a check with no way past it gets worked around by
/// worse means.
fn refuse_anonymous_exposure(
    address: &std::net::SocketAddr,
    open: bool,
    allow_anonymous: bool,
) -> Option<String> {
    if !open || allow_anonymous || address.ip().is_loopback() {
        return None;
    }

    let token_env = anamnesis_web::auth::TOKEN_ENV;
    Some(format!(
        "refusing to serve {address} with no token configured.\n\n\
         Everything this server holds — every prompt, every file path, every\n\
         summary written from them — would be readable by anything that can\n\
         reach that address.\n\n\
         Mint one with `anamnesis token`, then set {token_env} for this server\n\
         and for whatever runs the hooks. Or pass --allow-anonymous to serve\n\
         it open anyway."
    ))
}

/// The startup line for what the server accepts.
fn describe_serving_auth(auth: &anamnesis_web::Auth) -> String {
    if auth.is_open() {
        return "open — no token required".to_owned();
    }

    let named: Vec<String> = auth.named().map(ToString::to_string).collect();
    match named.len() {
        0 => "token required".to_owned(),
        _ => format!("token required ({})", named.join(", ")),
    }
}

/// Forward one hook event, and deliver the handoff when a session starts.
///
/// Never fails loudly. Hooks run inside someone's editing session, so a server
/// that is not running should cost them nothing more than a line on stderr.
fn cmd_hook(agent: &str, server: &str, token: Option<&str>) {
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

    let starting = is_starting(event.as_deref(), agent);

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

    let mut post = client
        .post(format!("{server}/hook"))
        .query(&[("agent", agent)])
        .header("content-type", "application/json")
        .body(payload.clone());
    if let Some(token) = token {
        post = post.bearer_auth(token);
    }
    let post = post.send();

    match post {
        Err(error) => {
            eprintln!("anamnesis: could not reach {server}: {error}");
            announce(
                agent,
                starting,
                &capture_notice(server, "could not be reached"),
            );
            return;
        }
        // A refused event is still an event lost.
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            let detail = response.text().unwrap_or_default();
            eprintln!(
                "anamnesis: server rejected event ({status}): {}",
                detail.trim()
            );
            announce(
                agent,
                starting,
                &capture_notice(server, &format!("refused the event ({status})")),
            );
            return;
        }
        Ok(_) => {}
    }

    // Only a starting session has anything to collect, and whatever comes back
    // goes to stdout, where the harness injects it into the model's context.
    //
    // Gemini CLI is the exception that shapes this: it requires stdout to be a
    // single JSON object and nothing else, so every event it fires gets one,
    // empty when there is nothing to say. Printing plain text there would not
    // fail loudly — it would fail as a parse error inside the harness, which
    // is the kind of failure this system is worst at explaining.
    if starting {
        let (session_id, cwd) = session_and_cwd(&payload);
        let mut request = client.get(format!("{server}/handoff")).query(&[
            ("agent", agent),
            ("session_id", &session_id),
            ("cwd", &cwd),
        ]);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        // The status is checked before the body is read, because this body
        // goes to stdout and the harness injects stdout into the model's
        // context. An error page printed as a handoff would not fail — it
        // would be believed.
        match request.send() {
            Ok(response) if response.status().is_success() => match response.text() {
                Ok(text) => print!("{}", handoff_reply(agent, &text)),
                Err(error) => {
                    eprintln!("anamnesis: handoff unavailable: {error}");
                    announce(
                        agent,
                        starting,
                        &handoff_notice("its reply could not be read"),
                    );
                }
            },
            Ok(response) => {
                let status = response.status();
                let detail = response.text().unwrap_or_default();
                eprintln!("anamnesis: handoff refused ({status}): {}", detail.trim());
                announce(
                    agent,
                    starting,
                    &handoff_notice(&format!("the server refused it ({status})")),
                );
            }
            Err(error) => {
                eprintln!("anamnesis: handoff unavailable: {error}");
                announce(
                    agent,
                    starting,
                    &handoff_notice("the server could not be reached"),
                );
            }
        }
    } else {
        // Every other event. Says nothing, except to the harness that wants an
        // object per call.
        announce(agent, false, "");
    }
}

/// Whether this event is the one that collects a handoff.
///
/// Three harnesses call it `SessionStart` and Cursor calls it `sessionStart`,
/// which is the whole of the difference — so it is compared without regard to
/// case rather than through a table. A harness that renames the moment
/// outright would need one, and this is where it would go.
fn is_starting(event: Option<&str>, _agent: &str) -> bool {
    event.is_some_and(|event| event.eq_ignore_ascii_case("sessionstart"))
}

/// Say something on stdout, in the shape this harness reads back.
///
/// Every path out of the hook goes through here, because stdout is a contract
/// and the contract does not lapse when something has gone wrong. Gemini CLI
/// parses stdout as one JSON object on **every** event it fires; before this,
/// a failed POST returned early and printed nothing at all, so the harness
/// least able to survive silence got silence exactly when the server was down.
///
/// Only a starting session carries a message. A notice on every tool call
/// would be the same sentence a hundred times in one context window, and the
/// place to learn that capture is down is the top of the session, once.
fn announce(agent: &str, starting: bool, text: &str) {
    if starting {
        print!("{}", handoff_reply(agent, text));
    } else if agent == "gemini-cli" {
        print!("{}", handoff_reply(agent, ""));
    }
}

/// What the model is told when the event it triggered never reached memory.
///
/// This exists because of how the failure looked from the outside: the server
/// was not running for four days, every hook in every session failed to
/// connect, and nothing anyone would see said so. The hook writes to stderr
/// and exits zero — deliberately, since a hook that fails loudly at the shell
/// level would interrupt hundreds of tool calls — and no harness surfaces
/// that. `anamnesis status` says it plainly, but only to someone who already
/// suspects.
///
/// stdout is the one channel a harness is guaranteed to read: it is how the
/// handoff reaches the model. So the notice goes there, at session start,
/// where whoever is working can be told in the same breath as the memory they
/// asked for.
///
/// It names itself. The standing rule is that a server's *response body* must
/// never be printed here — an error page injected as context would not fail,
/// it would be believed — and the way this stays on the right side of that
/// rule is that anamnesis wrote it, says so, and says what to do about it.
fn capture_notice(server: &str, reason: &str) -> String {
    format!(
        "[anamnesis] This session is NOT being recorded: the memory server at {server} {reason}. \
         Nothing said here will be remembered until it is running — `anamnesis status` says why, \
         `anamnesis serve` starts it."
    )
}

/// What the model is told when the event was recorded but the handoff was not.
///
/// Deliberately not the same sentence. Capture is working here, and saying
/// otherwise would send someone to restart a server that is already up. The
/// thing worth knowing is narrower and easy to misread: an empty handoff and a
/// handoff that could not be fetched look identical to a model, and one of them
/// means the last session left notes that are still waiting.
fn handoff_notice(reason: &str) -> String {
    format!(
        "[anamnesis] The previous session's handoff could not be collected: {reason}. This \
         session is starting without it and there may be notes still waiting; capture itself is \
         working."
    )
}

/// The handoff, in the shape the harness reads back.
///
/// Claude Code and Codex inject whatever a hook prints, so the handoff is
/// printed as it is and nothing is printed when there is none. Gemini CLI
/// parses stdout as one JSON object and rejects anything else, so it gets one
/// — carrying the handoff as `hookSpecificOutput.additionalContext`, or empty
/// when there is nothing to hand over.
///
/// The trailing newline matters only for the plain form, where stdout is
/// spliced into a prompt.
fn handoff_reply(agent: &str, handoff: &str) -> String {
    let handoff = handoff.trim();

    if agent == "gemini-cli" {
        let reply = if handoff.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": handoff,
                }
            })
        };
        return format!("{reply}\n");
    }

    // Cursor takes injected context as a top-level `additional_context`, and
    // unlike Gemini it does not insist on being spoken to: nothing to hand
    // over means nothing printed.
    if agent == "cursor" {
        if handoff.is_empty() {
            return String::new();
        }
        return format!("{}\n", serde_json::json!({ "additional_context": handoff }));
    }

    if handoff.is_empty() {
        return String::new();
    }
    format!("{handoff}\n")
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

/// Mint a token, and say where it goes.
///
/// Two variables rather than one, because they answer different questions.
/// `ANAMNESIS_TOKEN` is the secret this machine *presents*; `ANAMNESIS_TOKENS`
/// is the set a server *accepts*. On a single-user machine they hold the same
/// value and the distinction never comes up; on a shared server it is the
/// difference between "a token" and "whose token".
fn cmd_token(operator: Option<&str>) -> anyhow::Result<()> {
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
fn cmd_install_mcp(
    agent: &str,
    write: bool,
    config_path: Option<PathBuf>,
    repo: Option<PathBuf>,
) -> anyhow::Result<()> {
    // One harness, on purpose. Codex, Gemini CLI and Cursor all speak MCP and
    // all keep it somewhere of their own; writing a file whose shape has not
    // been checked against the harness that reads it is how somebody's
    // configuration gets a broken entry in it. The hooks for those three were
    // written only after their formats were confirmed, and this will be too.
    if agent != "claude-code" {
        println!("No MCP template for {agent} yet.");
        println!();
        println!("  Registered today: claude-code.");
        println!("  Codex, Gemini CLI and Cursor all speak MCP, but each keeps");
        println!("  its registration in a place and shape this has not checked.");
        println!("  Guessing would write a broken entry into a file somebody");
        println!("  else's tools read.");
        println!();
        println!("  The server itself is harness-agnostic: `anamnesis mcp --repo <dir>`");
        println!("  over stdio is all any of them need.");
        return Ok(());
    }

    let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("anamnesis"));
    let repo = match repo {
        Some(repo) => repo,
        None => std::env::current_dir()?,
    };
    let entry = mcp_config::server_entry(&binary, &repo);
    let document = serde_json::json!({ "mcpServers": { mcp_config::SERVER_NAME: entry.clone() } });

    if !write {
        println!(
            "Add this to {}:",
            mcp_config::config_path(std::path::Path::new(".")).display()
        );
        println!();
        println!("{}", serde_json::to_string_pretty(&document)?);
        println!();
        println!("Or run this again with `--write` to merge it in for you.");
        println!();
        println!("The MCP server does not need `anamnesis serve`: it opens the");
        println!("store directly. Hooks are the half that needs the server.");
        return Ok(());
    }

    let path = match config_path {
        Some(path) => path,
        None => mcp_config::config_path(&std::env::current_dir()?),
    };

    // Same rule as the hooks: a file that exists and does not parse is
    // somebody's configuration and possibly the only copy of it.
    let mut existing = match hooks::read_settings(&path) {
        Ok(config) => config,
        Err(error) => {
            println!("Could not read {} — {error}", path.display());
            println!();
            println!("Nothing was changed. Add this by hand:");
            println!();
            println!("{}", serde_json::to_string_pretty(&document)?);
            return Ok(());
        }
    };

    let outcome = mcp_config::register(&mut existing, mcp_config::SERVER_NAME, &entry);
    if outcome.changed() {
        hooks::write_settings(&path, &existing)?;
    }

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

    println!();
    println!("  Takes effect in the next session, not this one.");
    println!("  Then the agent can call memory_query rather than waiting to be");
    println!("  handed one summary at startup.");
    println!();
    // Said because the file is one a project may commit, and what is in it is
    // this machine's: an absolute path to a binary nobody else has.
    println!("  This names paths on this machine. `.mcp.json` is read from the");
    println!("  project root and is often committed — ignore it, or expect a");
    println!("  colleague's checkout to point at your home directory.");
    Ok(())
}

fn cmd_install_hooks(
    agent: &str,
    server: &str,
    write: bool,
    settings: Option<PathBuf>,
) -> anyhow::Result<()> {
    let Some(harness) = hooks::harness(agent) else {
        // A harness that cannot be wired this way gets a reason rather than a
        // "not yet": one of them is a permanent difference in how it extends,
        // and someone deserves to stop waiting for it.
        match hooks::cannot_wire(agent) {
            Some(reason) => {
                println!("{agent} cannot be wired by install-hooks.");
                println!();
                println!("  {reason}");
            }
            None => {
                println!("No hook template for {agent} yet.");
                println!();
                println!(
                    "  Wired today: {}",
                    hooks::HARNESSES
                        .iter()
                        .map(|harness| harness.agent)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
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

/// Score retrieval against a checked-in corpus.
///
/// Prints what each suite scored, and — with `--check` — refuses to exit zero
/// when one has fallen below the bar it sets for itself. The bar lives in the
/// suite file rather than here, so a change that costs recall shows up as a
/// number someone had to edit.
fn cmd_eval(
    suite: Option<&std::path::Path>,
    verbose: bool,
    check: bool,
    streams: bool,
    sweep: bool,
) -> anyhow::Result<()> {
    // Held still on purpose. Freshness is an input to nothing a suite scores,
    // and it can only be that way if two runs are handed the same instant.
    let now: Timestamp = "2026-01-01T00:00:00Z".parse()?;

    let suites: Vec<(String, anamnesis_evals::Suite)> = match suite {
        Some(path) => {
            let loaded = anamnesis_evals::Suite::load(path)?;
            vec![(path.display().to_string(), loaded)]
        }
        None => anamnesis_evals::builtin_suites()
            .into_iter()
            .map(|(name, source)| {
                anamnesis_evals::Suite::from_toml(source).map(|suite| (name.to_owned(), suite))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };

    if sweep {
        let grid = anamnesis_evals::default_grid();
        println!(
            "Sweeping {} settings over {} suite(s). Nothing here changes what ships.",
            grid.len(),
            suites.len()
        );
        println!();
        print_sweep(&anamnesis_evals::sweep(&suites, now, &grid)?, verbose);
        return Ok(());
    }

    let mut failed = 0usize;
    for (source, suite) in &suites {
        let report = anamnesis_evals::run(suite, now)?;
        print_report(&report, source, verbose);
        if streams {
            print_ablation(&anamnesis_evals::ablate(suite, now)?);
        }
        if !report.passed() {
            failed += 1;
        }
    }

    if failed > 0 && check {
        anyhow::bail!(
            "{failed} of {} suites scored below their thresholds",
            suites.len()
        );
    }
    Ok(())
}

/// One suite's results.
fn print_report(report: &anamnesis_evals::Report, source: &str, verbose: bool) {
    println!("🎯 {} — {}", report.name, report.description);
    println!(
        "   {} · {} pages · {} cases · scored over the first {}",
        source,
        report.pages,
        report.cases.len(),
        report.limit
    );
    println!();
    println!(
        "   MRR     {:.3}  {}",
        report.mrr,
        describe_bar(report.mrr, report.thresholds.min_mrr)
    );
    println!(
        "   Recall  {:.3}  {}",
        report.recall,
        describe_bar(report.recall, report.thresholds.min_recall)
    );

    // Printed whether or not anyone asked, and in two lists rather than one.
    // A suite that passes on average while a question goes unanswered is the
    // result most likely to be read as "fine" — and so is one whose answers
    // are all technically there, at the bottom of a page nobody scrolls.
    print_cases("Nothing relevant came back for:", report.misses());
    print_cases("Answered, but not near the top:", report.ranked_low());

    if verbose {
        println!();
        println!("   rank  query");
        for case in &report.cases {
            let rank = match case.score.rank {
                Some(rank) => format!("{rank:>4}"),
                None => "   —".to_owned(),
            };
            println!("   {rank}  {}", case.query);
        }
    }

    println!();
}

/// Every setting tried, best mean rank first.
///
/// Two things the table has to make impossible to miss. The row that ships
/// today is marked wherever it lands, because a list of alternatives with no
/// baseline says nothing about what changing costs. And the rows that clear
/// the acceptance rule — rank up and recall held on *every* suite — are marked
/// separately from the rows that merely sit at the top, because sorting by a
/// mean is exactly how a gain on one corpus pays for a loss on another.
fn print_sweep(report: &anamnesis_evals::SweepReport, verbose: bool) {
    /// Rows shown when the caller did not ask for all of them.
    const SHOWN: usize = 12;

    let mut header = String::from("        k  entity  links   auth  cover  depth");
    for suite in &report.suites {
        header.push_str(&format!("  {:>12}", truncate(suite, 12)));
    }
    header.push_str("     mean");
    println!("{header}");

    let baseline = report.baseline();
    let improvements = report.improvements();

    let mut shown: Vec<&anamnesis_evals::SweepPoint> = if verbose {
        report.points.iter().collect()
    } else {
        report.points.iter().take(SHOWN).collect()
    };
    // Always visible, however far down the table it sits.
    if !shown.iter().any(|point| point.is_default())
        && let Some(base) = baseline
    {
        shown.push(base);
    }

    for point in shown {
        let mut row = format!(
            "  {:>6.0}  {:>6.2}  {:>5.2}  {:>5.2}  {:>5.2}  {:>5}",
            point.tuning.rrf_k,
            point.tuning.entity,
            point.tuning.links,
            point.tuning.authority_exponent,
            point.tuning.entity_coverage,
            point.tuning.candidates
        );
        for score in &point.scores {
            row.push_str(&format!("  {:>5.3} {:>5.3}", score.mrr, score.recall));
        }
        row.push_str(&format!("  {:>7.3}", point.mean_mrr()));

        if point.is_default() {
            row.push_str("  ← ships today");
        } else if baseline.is_some_and(|base| point.improves_on(base)) {
            row.push_str("  ✓");
        }
        println!("{row}");
    }

    println!();
    match baseline {
        None => println!(
            "  The grid does not contain today's defaults, so none of this says what changing would cost."
        ),
        Some(_) => println!(
            "  {} of {} settings raise the mean rank on every suite without losing recall on any (✓).",
            improvements.len(),
            report.points.len()
        ),
    }
    println!("  A row winning here is not on its own a reason to adopt it: prefer the middle of a");
    println!(
        "  region that wins over a single spike, which this many questions cannot tell apart."
    );
    println!();
}

/// Cut a name down to fit a column, ending in an ellipsis when it is cut.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// What each stream contributes on its own.
fn print_ablation(ablation: &anamnesis_evals::Ablation) {
    println!("   stream     MRR    recall   only this stream finds");
    for stream in &ablation.streams {
        // The last column is the one that decides whether a stream stays: a
        // respectable average with nothing unique behind it means the other
        // streams already cover it.
        let unique = match stream.only_stream_to_find.len() {
            0 => "—".to_owned(),
            count => format!("{count}"),
        };
        println!(
            "   {:<9}  {:.3}  {:.3}    {}",
            stream.name, stream.mrr, stream.recall, unique
        );
    }

    for stream in &ablation.streams {
        for query in &stream.only_stream_to_find {
            println!("     only {} finds {query:?}", stream.name);
        }
    }

    if !ablation.found_by_none.is_empty() {
        println!();
        println!("   No single stream answered these — fusion is doing the work:");
        for query in &ablation.found_by_none {
            println!("     {query:?}");
        }
    }
    println!();
}

/// One list of cases, with the reason each is in the suite.
fn print_cases<'a>(heading: &str, cases: impl Iterator<Item = &'a anamnesis_evals::CaseOutcome>) {
    let cases: Vec<&anamnesis_evals::CaseOutcome> = cases.collect();
    if cases.is_empty() {
        return;
    }

    println!();
    println!("   {heading}");
    for case in cases {
        match case.score.rank {
            Some(rank) => println!("     [{rank}] {:?}", case.query),
            None => println!("     [—] {:?}", case.query),
        }
        if !case.note.is_empty() {
            println!("         {}", case.note);
        }
    }
}

/// How a measurement sits against the bar the suite set for itself.
///
/// A suite that set no bar is reported without one rather than as a pass: it
/// was never being gated, and a tick would say it was.
fn describe_bar(value: f64, bar: f64) -> String {
    if bar <= 0.0 {
        return "(no threshold)".to_owned();
    }
    if value >= bar {
        format!("(bar {bar:.3}) ok")
    } else {
        format!("(bar {bar:.3}) BELOW")
    }
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

    let now = Timestamp::now();
    let report = reindex::rebuild(&store, &wiki, &raw, &scope, now)?;

    // The shared scope is rebuilt with the project, because a rebuild that
    // left it out would drop the index rows for pages every project in the
    // workspace can see — and nothing else would ever put them back.
    let global = global_scope(&scope, &data);
    let shared = if data.wiki_global(&scope.scope.workspace).exists() {
        Some(reindex::rebuild(&store, &wiki, &raw, &global, now)?)
    } else {
        None
    };

    println!("  {} page(s) indexed", report.pages);
    if let Some(shared) = &shared {
        println!(
            "  {} page(s) indexed in {}",
            shared.pages, global.scope.project
        );
    }
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
    if report.removed > 0 {
        println!(
            "  {} forgotten — no longer in the wiki",
            plural(report.removed as i64, "page")
        );
    }
    if report.skipped_removal {
        println!();
        println!("  ⚠ No wiki directory at that path, so nothing was forgotten.");
        println!("    An index with rows and a scope with no directory usually");
        println!("    means this ran against the wrong data dir or project.");
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
fn cmd_handoff(
    workstream: Option<String>,
    operator: Option<String>,
    discard: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
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

    // Refused rather than ignored: someone who passes `--operator` on a
    // project that keys one slot is asking a question about a separation that
    // does not exist, and answering with the shared slot would look like an
    // answer about theirs.
    if operator.is_some() && !scope.slots.per_user {
        anyhow::bail!(
            "this project keeps one handoff slot; --operator needs `[slots] per_user = true` in {}",
            scope
                .marker
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| anamnesis_core::scope::MARKER_FILE.to_owned())
        );
    }

    let operator = match operator.as_deref().map(str::trim) {
        Some(name) => Some(anamnesis_core::scope::OperatorName::parse(name)?),
        None => None,
    };

    let slot =
        anamnesis_core::handoff::Slot::for_workstream(workstream_id).for_operator(operator.clone());

    if discard {
        // Read back rather than reported as a bare success: a note is being
        // thrown away, and the person doing it should see what it said in case
        // it was not the one they meant.
        return match store.discard_handoff(scope.project_id, &slot)? {
            Some(body) => {
                println!(
                    "🗑  Discarded the handoff{}:",
                    describe_slot(&workstream, &operator)
                );
                println!();
                println!("{body}");
                println!();
                println!("  The next session will start without one. The row is kept,");
                println!("  marked expired, so what was written is still on record.");
                Ok(())
            }
            None => {
                println!(
                    "Nothing waiting{} — nothing to discard.",
                    describe_slot(&workstream, &operator)
                );
                Ok(())
            }
        };
    }

    match store.peek_handoff(scope.project_id, &slot)? {
        Some(body) => {
            println!(
                "📋 Pending handoff{}:",
                describe_slot(&workstream, &operator)
            );
            println!();
            println!("{body}");
        }
        None => println!(
            "Nothing waiting{} — the last session left no handoff, or it was already claimed.",
            describe_slot(&workstream, &operator)
        ),
    }
    Ok(())
}

/// Name the slot that was looked in, when it was not the only one.
///
/// Said every time it is not the shared slot, because "nothing waiting" and
/// "nothing waiting in this one slot of several" are different answers, and
/// the second one is the one that sends someone looking in the right place.
fn describe_slot(
    workstream: &Option<String>,
    operator: &Option<anamnesis_core::scope::OperatorName>,
) -> String {
    match (workstream, operator) {
        (None, None) => String::new(),
        (Some(slug), None) => format!(" for workstream {slug}"),
        (None, Some(operator)) => format!(" for {operator}"),
        (Some(slug), Some(operator)) => format!(" for {operator} in workstream {slug}"),
    }
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
        // Both are absent for the ordinary session, and both change what the
        // line means when they are not: which thread of work it belongs to,
        // and whose it was.
        let workstream = match &session.workstream {
            Some(slug) => format!(" · {slug}"),
            None => String::new(),
        };
        let operator = match &session.operator {
            Some(operator) => format!(" · {operator}"),
            None => String::new(),
        };
        println!(
            "{short}  {when}  {:<12} {:<7} {} obs{workstream}{operator}",
            session.agent, session.state, session.observation_count
        );
    }
    Ok(())
}

/// Remove a page somebody named, from the wiki and the index.
///
/// The counterpart to `sweep`, which forgets what decayed. This forgets what
/// was *wrong*: a page written from a bad reply, a note that turned out to be
/// untrue, a duplicate. Until now the only ways out were to wait for decay —
/// which never comes for a pinned or durable page — or to delete the file by
/// hand and hope the watcher was running to notice.
///
/// Deliberately not gated behind `--apply`, unlike the sweep. A sweep proposes
/// a judgement over pages nobody named, and the report is where that judgement
/// is checked; here a person has named the page. What the command owes them
/// instead is to say what it removed and where it went, which the wiki's git
/// history makes answerable.
fn cmd_forget(paths: &[String], data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let wiki = Wiki::open(data.wiki())?;

    // Every path is resolved before anything is removed. Forgetting two pages
    // and then refusing the third would leave the caller to work out which of
    // the three names was the typo.
    let mut doomed = Vec::with_capacity(paths.len());
    for path in paths {
        let page_path = anamnesis_core::page::PagePath::parse(path)?;
        if !wiki.exists(&scope.scope, &page_path) {
            anyhow::bail!(
                "no page at {page_path} — looked in {}",
                wiki.locate(&scope.scope, &page_path).display()
            );
        }
        let title = wiki
            .read_page(&scope.scope, &page_path)
            .map(|page| page.frontmatter.title)
            // A page whose frontmatter no longer parses is exactly the kind
            // worth removing, so it is described by its path and removed.
            .unwrap_or_else(|_| "(unreadable frontmatter)".to_owned());
        doomed.push((page_path, title));
    }

    println!("🗑  Forgetting from {}", scope.scope);
    println!();
    for (path, title) in &doomed {
        println!("  {path}");
        println!("     {title}");
    }
    println!();

    // Index first, then the wiki — the order `sweep` chose, for its reason: an
    // interruption here leaves a page briefly unfindable and wholly
    // recoverable by `reindex`, where the reverse order leaves the index
    // pointing at markdown that is gone and no rebuild can repair.
    let mut rows = 0;
    for (path, _) in &doomed {
        if store.delete_page(anamnesis_core::ids::PageId::derive(scope.project_id, path))? {
            rows += 1;
        }
    }

    let removed: Vec<anamnesis_core::page::PagePath> =
        doomed.iter().map(|(path, _)| path.clone()).collect();
    let message = forget_commit_message(&doomed);
    let commit = wiki
        .delete_pages(&scope.scope, &removed, &message)
        .map_err(|error| {
            anyhow::anyhow!(
                "{error}\n\
                 {rows} index row(s) were already dropped, but the wiki still holds every page. \
                 Nothing is lost: run `anamnesis reindex` to put the index back."
            )
        })?;

    println!("  {} page(s), {rows} index row(s).", doomed.len());
    match commit {
        Some(commit) => {
            println!("  Committed {}.", &commit[..commit.len().min(8)]);
            println!();
            println!("  Still recoverable — the wiki is a git repository:");
            println!("    git -C {} show {commit}", data.wiki().display());
        }
        None => println!("  Nothing for git to record."),
    }
    Ok(())
}

/// What the wiki's history says about a deliberate removal.
///
/// Named pages and a person's decision, rather than the sweep's decay scores:
/// once the pages are gone this message is the only remaining account of what
/// was here, and "someone decided" is the part that would otherwise be lost.
fn forget_commit_message(doomed: &[(anamnesis_core::page::PagePath, String)]) -> String {
    let mut message = format!("forget: {} page(s) removed on request\n", doomed.len());
    for (path, title) in doomed {
        message.push_str(&format!("\n- {path} — {title}"));
    }
    message.push('\n');
    message
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
        assert!(describe_memory(2, 1, true, None).contains("handoff waiting"));
        assert!(describe_memory(2, 1, false, None).contains("no handoff waiting"));
    }

    #[test]
    fn counts_are_pluralised_rather_than_parenthesised() {
        assert_eq!(plural(0, "page"), "0 pages");
        assert_eq!(plural(1, "page"), "1 page");
        assert_eq!(plural(2, "page"), "2 pages");
    }

    /// The line exists to answer "is my work reaching memory", so the case
    /// where it is not — a hook whose token the server refuses — has to read
    /// as a problem and name what to check.
    #[test]
    fn a_rejected_token_says_which_variable_is_wrong() {
        let line = describe_auth(&AuthState::Rejected, true);
        assert!(line.contains("rejected"), "{line}");
        assert!(line.contains(anamnesis_web::auth::TOKEN_ENV), "{line}");
    }

    /// Missing and wrong are different fixes: one is "set the variable", the
    /// other is "you set it to the wrong thing".
    #[test]
    fn no_token_at_all_is_reported_differently_from_a_wrong_one() {
        let missing = describe_auth(&AuthState::Rejected, false);
        let wrong = describe_auth(&AuthState::Rejected, true);
        assert_ne!(missing, wrong);
        assert!(missing.contains("has no token"), "{missing}");
    }

    #[test]
    fn an_open_server_is_reported_as_open_rather_than_as_working() {
        let line = describe_auth(&AuthState::Open, false);
        assert!(line.contains("not required"), "{line}");
        assert!(line.contains("can read this memory"), "{line}");
    }

    #[test]
    fn an_accepted_token_names_the_operator_when_it_has_one() {
        assert!(
            describe_auth(&AuthState::Accepted(Some("alice".to_owned())), true).contains("alice")
        );
        assert!(describe_auth(&AuthState::Accepted(None), true).contains("accepted"));
    }

    /// A server that did not answer is not a server that is open. Saying
    /// "not required" here would be a false all-clear.
    #[test]
    fn a_silent_server_is_not_reported_as_unprotected() {
        let line = describe_auth(&AuthState::Unknown, false);
        assert!(line.contains("unknown"), "{line}");
        assert!(!line.contains("not required"), "{line}");
    }

    fn address(raw: &str) -> std::net::SocketAddr {
        raw.parse().expect("address")
    }

    /// Loopback is where every single-user install lives, and where the port
    /// is the boundary. Refusing there would break them all for no gain.
    #[test]
    fn an_open_server_on_loopback_is_allowed_to_start() {
        assert!(refuse_anonymous_exposure(&address("127.0.0.1:8080"), true, false).is_none());
        assert!(refuse_anonymous_exposure(&address("[::1]:8080"), true, false).is_none());
    }

    #[test]
    fn an_open_server_on_a_network_address_is_refused_and_told_how() {
        let refusal = refuse_anonymous_exposure(&address("0.0.0.0:8080"), true, false)
            .expect("should refuse");
        assert!(refusal.contains("anamnesis token"), "{refusal}");
        assert!(refusal.contains("--allow-anonymous"), "{refusal}");
    }

    /// The refusal is about being unguarded, not about the address: a server
    /// with tokens configured may bind wherever it likes.
    #[test]
    fn a_guarded_server_may_serve_any_address() {
        assert!(refuse_anonymous_exposure(&address("0.0.0.0:8080"), false, false).is_none());
    }

    /// "Behind a proxy that authenticates" is a real deployment, and a check
    /// with no way past it gets worked around by worse means.
    #[test]
    fn the_refusal_can_be_overridden_deliberately() {
        assert!(refuse_anonymous_exposure(&address("0.0.0.0:8080"), true, true).is_none());
    }

    #[test]
    fn the_startup_line_names_the_operators_it_will_accept() {
        let auth = anamnesis_web::Auth::parse(None, Some("alice=alpha,bob=beta")).expect("parse");
        let line = describe_serving_auth(&auth);
        assert!(line.contains("alice"), "{line}");
        assert!(line.contains("bob"), "{line}");

        let shared = anamnesis_web::Auth::parse(Some("alpha"), None).expect("parse");
        assert_eq!(describe_serving_auth(&shared), "token required");
        assert!(describe_serving_auth(&anamnesis_web::Auth::open()).contains("open"));
    }

    fn an_operator(name: &str) -> anamnesis_core::scope::OperatorName {
        anamnesis_core::scope::OperatorName::parse(name).expect("valid operator")
    }

    /// On a shared server, "no handoff waiting" without a name is the sentence
    /// that makes someone believe their memory is empty when it is somebody
    /// else's slot they are looking at.
    #[test]
    fn the_memory_line_names_the_slot_it_looked_in() {
        let alice = an_operator("alice");
        let line = describe_memory(2, 1, false, Some(&alice));
        assert!(line.contains("no handoff waiting for alice"), "{line}");

        // And says nothing extra where there is only one slot to look in.
        let shared = describe_memory(2, 1, false, None);
        assert!(shared.ends_with("no handoff waiting"), "{shared}");
    }

    #[test]
    fn a_peeked_slot_is_named_only_when_it_is_not_the_only_one() {
        assert_eq!(describe_slot(&None, &None), "");
        assert_eq!(
            describe_slot(&Some("auth".to_owned()), &None),
            " for workstream auth"
        );
        assert_eq!(
            describe_slot(&None, &Some(an_operator("alice"))),
            " for alice"
        );
        assert_eq!(
            describe_slot(&Some("auth".to_owned()), &Some(an_operator("alice"))),
            " for alice in workstream auth"
        );
    }

    /// The name the server gave is the one the slot is keyed by, so a name
    /// this build could not use as a key must not be treated as one.
    #[test]
    fn an_unusable_operator_name_is_no_operator_at_all() {
        assert_eq!(
            AuthState::Accepted(Some("alice".to_owned())).operator(),
            Some(an_operator("alice"))
        );
        assert_eq!(
            AuthState::Accepted(Some("Not A Name".to_owned())).operator(),
            None
        );
        assert_eq!(AuthState::Accepted(None).operator(), None);
        assert_eq!(AuthState::Open.operator(), None);
    }

    /// The finding this exists for: refinery logs every migration's whole SQL
    /// text at info, and `anamnesis init` saying where memory now lives
    /// scrolls away under it on the first run.
    #[test]
    fn the_default_filter_quiets_the_migration_sql() {
        let filter = default_filter(false);
        assert!(filter.starts_with("info"), "{filter}");
        assert!(filter.contains("refinery_core=warn"), "{filter}");
    }

    /// A migration that fails halfway is exactly when the statement is worth
    /// seeing, so debugging must not inherit the muzzle.
    #[test]
    fn debug_logging_still_shows_it() {
        assert!(!default_filter(true).contains("refinery_core=warn"));
    }

    /// The summary line is the only feedback that a flag was understood, so
    /// every flag that changes how a page is treated has to appear in it.
    #[test]
    fn the_summary_names_what_the_page_was_written_as() {
        let mut frontmatter =
            anamnesis_core::page::Frontmatter::new("t", Vec::new()).expect("frontmatter");
        assert_eq!(describe_page(&frontmatter), "episodic");

        frontmatter.tier = anamnesis_core::page::Tier::Semantic;
        frontmatter.canonical = true;
        frontmatter.pinned = true;
        frontmatter.status = anamnesis_core::page::PageStatus::Historical;
        frontmatter.entities = vec![
            anamnesis_core::page::Entity::parse("SQLite").expect("entity"),
            anamnesis_core::page::Entity::parse("recall").expect("entity"),
        ];

        let line = describe_page(&frontmatter);
        assert!(line.contains("semantic"), "{line}");
        assert!(line.contains("historical"), "{line}");
        assert!(line.contains("canonical"), "{line}");
        assert!(line.contains("pinned"), "{line}");
        assert!(line.contains("SQLite, recall"), "{line}");
    }

    /// The default status is the ordinary case and saying it adds nothing;
    /// every other status changes whether an agent answers from the page.
    #[test]
    fn an_ordinary_page_is_described_by_its_tier_alone() {
        let frontmatter =
            anamnesis_core::page::Frontmatter::new("t", Vec::new()).expect("frontmatter");
        assert!(!describe_page(&frontmatter).contains("active"));
    }

    /// Once the pages are gone this message is the only account of what was
    /// here, so it has to carry both halves: which pages, and that a person
    /// asked for it rather than a decay score deciding.
    #[test]
    fn the_forget_commit_names_every_page_and_says_who_asked() {
        let doomed = vec![
            (
                anamnesis_core::page::PagePath::parse("notes/wrong.md").expect("path"),
                "A note that turned out to be untrue".to_owned(),
            ),
            (
                anamnesis_core::page::PagePath::parse("sessions/2026-08-29-abcd.md").expect("path"),
                "2026-08-29: a session".to_owned(),
            ),
        ];

        let message = forget_commit_message(&doomed);

        assert!(message.starts_with("forget: 2 page(s) removed on request"));
        assert!(
            message.contains("notes/wrong.md — A note that turned out"),
            "{message}"
        );
        assert!(
            message.contains("sessions/2026-08-29-abcd.md — 2026-08-29: a session"),
            "{message}"
        );
    }

    /// The failure this whole notice exists for: the server was down for four
    /// days, every hook failed to connect, and nothing anyone would read said
    /// so. Whatever else it says, it has to say that nothing is being kept.
    #[test]
    fn the_capture_notice_says_that_nothing_is_being_recorded() {
        let notice = capture_notice("http://127.0.0.1:8080", "could not be reached");

        assert!(notice.contains("NOT being recorded"), "{notice}");
        assert!(notice.contains("http://127.0.0.1:8080"), "{notice}");
        assert!(notice.contains("anamnesis serve"), "{notice}");
        assert!(
            !notice.contains('\n'),
            "one line, not a paragraph: {notice}"
        );
    }

    /// It is injected where a handoff goes, so it has to be impossible to read
    /// as one. The standing rule is that a server's response body is never
    /// printed there — this stays on the right side of it by being written
    /// here, and saying whose words they are.
    #[test]
    fn a_notice_names_itself_rather_than_passing_as_memory() {
        assert!(capture_notice("http://x", "could not be reached").starts_with("[anamnesis]"));
        assert!(handoff_notice("the server could not be reached").starts_with("[anamnesis]"));
    }

    /// Two failures that need different sentences. Telling someone capture is
    /// dead when only the handoff failed sends them to restart a server that
    /// is already running, and the notice would be its own false alarm.
    #[test]
    fn a_failed_handoff_does_not_claim_capture_is_broken() {
        let notice = handoff_notice("the server refused it (401 Unauthorized)");

        assert!(notice.contains("401 Unauthorized"), "{notice}");
        assert!(notice.contains("capture itself is working"), "{notice}");
        assert!(!notice.contains("NOT being recorded"), "{notice}");
    }

    /// A notice is delivered the same way a handoff is, so it survives every
    /// harness's idea of stdout — including the one that parses it as JSON.
    #[test]
    fn a_notice_reaches_every_harness_in_its_own_shape() {
        let notice = capture_notice("http://127.0.0.1:8080", "could not be reached");

        let gemini: serde_json::Value =
            serde_json::from_str(&handoff_reply("gemini-cli", &notice)).expect("valid JSON");
        assert!(
            gemini["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .expect("context")
                .contains("NOT being recorded")
        );

        let cursor: serde_json::Value =
            serde_json::from_str(&handoff_reply("cursor", &notice)).expect("valid JSON");
        assert!(
            cursor["additional_context"]
                .as_str()
                .expect("context")
                .contains("NOT being recorded")
        );

        assert!(handoff_reply("claude-code", &notice).starts_with("[anamnesis]"));
    }

    /// The harness that shapes this: Gemini CLI parses stdout as one JSON
    /// object and rejects anything else, so plain text there is not a
    /// degraded handoff — it is a parse error inside somebody's agent.
    #[test]
    fn gemini_gets_one_json_object_whether_or_not_there_is_a_handoff() {
        let with = handoff_reply("gemini-cli", "Last request: wire it up\n");
        let parsed: serde_json::Value = serde_json::from_str(&with).expect("valid JSON");
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"],
            "Last request: wire it up"
        );
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );

        let without: serde_json::Value =
            serde_json::from_str(&handoff_reply("gemini-cli", "")).expect("valid JSON");
        assert_eq!(without, serde_json::json!({}));
    }

    /// Everything else injects whatever the hook printed, so the handoff is
    /// printed as it is — and nothing at all when there is none, because an
    /// empty line is still a line in somebody's context.
    #[test]
    fn the_other_harnesses_get_the_handoff_as_it_is() {
        assert_eq!(
            handoff_reply("claude-code", "Last request: wire it up"),
            "Last request: wire it up\n"
        );
        assert_eq!(handoff_reply("codex", "  "), "");
        assert_eq!(handoff_reply("claude-code", ""), "");
    }

    /// A refused or unreachable handoff must not leave Gemini with an empty
    /// stdout it was told never to expect.
    #[test]
    fn a_failed_handoff_still_leaves_gemini_a_valid_object() {
        let reply = handoff_reply("gemini-cli", "");
        serde_json::from_str::<serde_json::Value>(&reply).expect("valid JSON");
    }

    /// Cursor takes context back as a top-level `additional_context`, and
    /// unlike Gemini it is content with silence when there is none.
    #[test]
    fn cursor_gets_its_own_field_and_nothing_when_there_is_nothing() {
        let reply = handoff_reply("cursor", "Last request: wire it up");
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["additional_context"], "Last request: wire it up");
        assert_eq!(handoff_reply("cursor", ""), "");
    }

    #[test]
    fn only_a_starting_session_collects_a_handoff() {
        assert!(is_starting(Some("SessionStart"), "claude-code"));
        assert!(!is_starting(Some("SessionEnd"), "claude-code"));
        assert!(!is_starting(None, "gemini-cli"));
        // Cursor spells it differently, and it is still the same moment.
        assert!(is_starting(Some("sessionStart"), "cursor"));
    }
}
