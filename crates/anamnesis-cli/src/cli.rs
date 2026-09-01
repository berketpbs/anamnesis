//! What the command line accepts.
//!
//! Only the shape of the arguments lives here: every `Commands` variant is a
//! description of what a person can ask for, and none of them knows how it is
//! carried out. Kept apart from `main.rs` because the two are read for
//! different reasons — this file answers "what can I type", and the modules
//! beside it answer "what happens when I do".
//!
//! The doc comment on a variant or a field is not a comment. Clap prints it as
//! the help text, so the wording is user-facing and the first line of each is
//! written to be read on its own.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::bootstrap;

/// The top-level command line.
#[derive(Parser)]
#[command(name = "anamnesis")]
#[command(about = "Long-term memory for AI coding agents")]
#[command(version)]
#[command(
    long_about = "Anamnesis preserves context across AI agent sessions through a persistent wiki.
Quit Claude Code mid-task, start Codex in the same directory, and the next agent
receives a bounded handoff with previous decisions, attempted approaches, and open questions."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Root of the anamnesis data directory (wiki, raw, db, models, logs)
    #[arg(long, global = true, env = "ANAMNESIS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Enable debug logging
    #[arg(long, global = true)]
    pub debug: bool,
}

/// Everything `anamnesis` can be asked to do.
#[derive(Subcommand)]
pub enum Commands {
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

        /// Do not serve the wiki browser at /ui
        ///
        /// The browser is the one part of this server that can read the whole
        /// of memory: the API accepts events and delivers one handoff, and
        /// neither hands back an arbitrary page. Turning it off leaves the
        /// capture path exactly as it was.
        #[arg(long)]
        no_ui: bool,

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

        /// Ask what would be recorded, and record nothing
        ///
        /// The way to check that capture is working without leaving a session
        /// behind. Reads a payload on stdin if one is piped, and makes one up
        /// for this directory otherwise. Unlike the hook itself, this exits
        /// non-zero when memory would not record.
        #[arg(long)]
        probe: bool,
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

        /// Score with the embedding stream switched on
        ///
        /// Off by default, as it is in production: the model is a download
        /// this should not require to say anything about the other three
        /// streams. With it, the corpus is embedded page by page and every
        /// question with the same model.
        #[arg(long)]
        embed: bool,

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

    /// Forget a session, its observations, and its transcript
    ///
    /// For a session that was never a session: a hook fired by hand to check
    /// that capture is alive is recorded like any other, and stays. Reports
    /// what would go and changes nothing unless `--apply` is given, because
    /// unlike a page a transcript is not in any git history.
    ForgetSession {
        /// Session ids, or any unambiguous prefix, as `sessions` prints them
        #[arg(required = true)]
        sessions: Vec<String>,

        /// Actually forget them, instead of only reporting them
        #[arg(long)]
        apply: bool,
    },

    /// Write the whole of memory to one archive
    ///
    /// The wiki carries its own git history, but `raw/` does not: the
    /// observations a page was compiled from live in exactly one place. This
    /// takes the index, the transcripts and the wiki — `models/` and `logs/`
    /// are left out, being a download and one machine's afternoons.
    Backup {
        /// Where to write the archive (default: ./anamnesis-backup-<stamp>.tar.gz)
        #[arg(long)]
        out: Option<PathBuf>,
    },

    /// Put an archive back, after saying what that would mean
    Restore {
        /// Archive written by `anamnesis backup`
        archive: PathBuf,

        /// Actually restore, instead of only reporting what would be restored
        #[arg(long)]
        apply: bool,

        /// Restore into a data directory that already holds memory
        ///
        /// Without this, a directory with an index, a wiki, or transcripts in
        /// it is left exactly as it was: restoring is the one operation here
        /// that running the other one cannot undo.
        #[arg(long)]
        force: bool,
    },

    /// Start a harness here, with memory wired
    ///
    /// Checks before it starts: the server has to be answering and this
    /// harness's hooks have to point at anamnesis. A session that will not be
    /// recorded is the session worth not starting — that failure has cost this
    /// repository two afternoons, both discovered days later.
    Run {
        /// Which harness to start
        #[arg(value_name = "AGENT")]
        agent: String,

        /// Executable to run, when the harness is called something else here
        #[arg(long)]
        program: Option<String>,

        /// Server the harness's hooks should deliver to
        #[arg(
            long,
            env = "ANAMNESIS_SERVER",
            default_value = "http://127.0.0.1:8080"
        )]
        server: String,

        /// Token to pass on, when the server requires one
        #[arg(long, env = anamnesis_web::auth::TOKEN_ENV, hide_env_values = true)]
        token: Option<String>,

        /// Start even though nothing will be recorded
        #[arg(long)]
        anyway: bool,

        /// Everything after `--`, passed to the harness untouched
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Start whichever harness this project last used
    ///
    /// The memory travels either way — the handoff is waiting for whoever
    /// starts next, and every harness reads the same one — so this is about
    /// picking a thread back up rather than about moving memory between tools.
    Continue {
        /// Executable to run, when the harness is called something else here
        #[arg(long)]
        program: Option<String>,

        /// Server the harness's hooks should deliver to
        #[arg(
            long,
            env = "ANAMNESIS_SERVER",
            default_value = "http://127.0.0.1:8080"
        )]
        server: String,

        /// Token to pass on, when the server requires one
        #[arg(long, env = anamnesis_web::auth::TOKEN_ENV, hide_env_values = true)]
        token: Option<String>,

        /// Start even though nothing will be recorded
        #[arg(long)]
        anyway: bool,

        /// Everything after `--`, passed to the harness untouched
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Measure how many events a second this machine can record
    ///
    /// A hook runs before every tool call and gives up after a second, and on
    /// a shared server every session's events arrive at the same index. This
    /// answers "will that hold" with a number instead of an argument. It runs
    /// against a temporary data directory and writes nothing to this
    /// project's memory.
    Bench {
        /// How many events to record
        #[arg(long, default_value_t = crate::bench::DEFAULT_EVENTS)]
        events: usize,
    },

    /// Remove this project's memory entirely
    ///
    /// The end of the family `forget` and `forget-session` start, for the
    /// memory that is wrong rather than incomplete. Pages leave as a git
    /// commit and stay in the wiki's history; the transcripts do not come
    /// back at all.
    Purge {
        /// Actually remove it, instead of only reporting what would go
        #[arg(long)]
        apply: bool,
    },

    /// Show what has been changed by hand, newest first
    ///
    /// Capture is recorded as sessions, not here. This is the log of
    /// deliberate changes — pages written or forgotten, sessions removed,
    /// handoffs claimed, proposals carried out — and it is what makes "why
    /// does this page say that now" a question with an answer.
    Audit {
        /// How many changes to show
        #[arg(long, default_value = "20")]
        limit: usize,

        /// Every project this server holds, not only this one
        #[arg(long)]
        everywhere: bool,
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
