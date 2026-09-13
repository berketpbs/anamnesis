//! CLI entry point for anamnesis.
//!
//! Three things happen here and nothing else: the arguments are parsed, the
//! process is given somewhere to log, and one command is called. Every command
//! lives beside the thing it acts on — `capture` beside the queue it drains,
//! `sweep` beside the decay it reports, `status` beside the probes it phrases
//! — so that reading this file tells you what anamnesis can be asked to do,
//! and reading one module tells you how one answer is arrived at.

mod abstracts;
mod archive;
mod audit;
mod bench;
mod bootstrap;
mod capture;
mod cli;
mod doctor;
mod evals;
mod format;
mod hooks;
mod improve;
mod keys;
mod lint;
mod mcp_config;
mod opencode;
mod pages;
mod project;
mod purge;
mod reconsolidate;
mod redact;
mod reindex;
mod rename;
mod run;
mod serve;
mod service;
mod sessions;
mod settings;
mod setup;
mod spool;
mod status;
mod sweep;
mod uninstall;

use abstracts::cmd_abstracts;
use archive::{cmd_backup, cmd_restore};
use audit::cmd_audit;
use bench::cmd_bench;
use bootstrap::cmd_bootstrap;
use capture::{cmd_hook, cmd_probe};
use cli::{Cli, Commands};
use evals::cmd_eval;
use improve::cmd_improve;
use pages::{PageOptions, cmd_forget, cmd_search, cmd_show_page, cmd_write_page};
use purge::cmd_purge;
use reconsolidate::cmd_reconsolidate;
use reindex::cmd_reindex;
use rename::cmd_rename;
use run::{cmd_continue, cmd_run};
use serve::{cmd_mcp, cmd_serve};
use sessions::{cmd_forget_session, cmd_handoff, cmd_sessions};
use setup::{cmd_init, cmd_install_hooks, cmd_install_mcp, cmd_token};
use status::cmd_status;
use sweep::cmd_sweep;
use uninstall::cmd_uninstall;

use anamnesis_core::datadir::DataDir;
use clap::Parser;
use std::path::PathBuf;

/// Stack for the thread every command runs on.
///
/// In a debug build `run`'s frame holds the locals of every command's arm at
/// once, and Windows gives a program's main thread one megabyte. Adding the
/// `key` command took the frame past it: every command, `--version` included,
/// died with a stack overflow before an argument was parsed. A release build
/// fits, CI never runs `main`, and a debug build on Windows is what somebody
/// working on this runs — so the stack is chosen here rather than by the
/// platform, with room for the commands still to come.
const STACK: usize = 16 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    let worker = std::thread::Builder::new()
        .name("anamnesis".to_owned())
        .stack_size(STACK)
        .spawn(run)?;
    match worker.join() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn run() -> anyhow::Result<()> {
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
    let writes_a_log = log_file.is_some();

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

    // A panic is the one way the server stops that it did not write down. The
    // standard hook prints to stderr, and stderr belongs to whatever started
    // the process: a terminal that is gone by the time anyone asks, or a
    // service manager that discards it. On the machine this project runs on, a
    // PowerShell wrapper existed partly to redirect stderr into a file so a
    // panic would leave anything at all. The log file is where the server's
    // own account already goes, so the panic goes there too — and the
    // standard hook still runs after it, so a terminal sees what it always did.
    //
    // Global, so a panic in a spawned task that the server survives is written
    // down as well; that one used to leave nothing anywhere.
    if writes_a_log {
        let standard = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let (message, location) = describe_panic(info);
            tracing::error!(
                %message,
                %location,
                thread = std::thread::current().name().unwrap_or("unnamed"),
                "panicked"
            );
            standard(info);
        }));
    }

    // Read before any command, so every one of them sees the same settings:
    // the server, the MCP server a harness starts, and the CLI asked about
    // either. A line that was typed and not applied is said here — and the
    // server will not start with one, since it is the process that runs
    // unattended. Not the hook: it answers on stdout and must never fail.
    let loaded = settings::init(cli.data_dir.clone());
    if !loaded.problems.is_empty() {
        if matches!(cli.command, Commands::Serve { .. }) {
            for problem in &loaded.problems {
                tracing::error!(%problem, "settings file refused a line");
            }
            anyhow::bail!(
                "{} has lines that were not applied:\n  {}",
                loaded.path.display(),
                loaded.problems.join("\n  ")
            );
        }
        if !matches!(cli.command, Commands::Hook { .. }) {
            for problem in &loaded.problems {
                eprintln!("anamnesis: {problem}");
            }
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
        Commands::Doctor { server } => {
            doctor::cmd_doctor(&server, cli.data_dir.clone())?;
        }
        Commands::Lint => {
            lint::cmd_lint(cli.data_dir.clone())?;
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
            no_ui,
            allow_anonymous,
        } => {
            cmd_serve(
                &bind,
                port,
                anamnesis_web::ServeOptions {
                    watch_wiki: !no_watch,
                    ui: !no_ui,
                },
                allow_anonymous,
                cli.data_dir.clone(),
            )?;
        }
        Commands::Hook {
            agent,
            server,
            token,
            probe,
        } => {
            if probe {
                cmd_probe(&agent, &server, token.as_deref())?;
            } else {
                cmd_hook(&agent, &server, token.as_deref(), cli.data_dir.clone());
            }
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
        Commands::Service { action } => match action {
            cli::ServiceAction::Install {
                write,
                binary,
                port,
            } => service::cmd_service_install(write, binary, port, cli.data_dir.clone())?,
            cli::ServiceAction::Uninstall { write } => service::cmd_service_uninstall(write)?,
            cli::ServiceAction::Status { port } => {
                service::cmd_service_status(port, cli.data_dir.clone())?
            }
        },
        Commands::Key { action } => match action {
            cli::KeyAction::Set { name, stdin } => keys::cmd_key_set(&name, stdin)?,
            cli::KeyAction::List => keys::cmd_key_list()?,
            cli::KeyAction::Forget { name } => keys::cmd_key_forget(&name)?,
        },
        Commands::Eval {
            suite,
            pages_from,
            scope,
            verbose,
            check,
            streams,
            sweep,
            k_sensitivity,
            compare,
            embed,
        } => {
            cmd_eval(
                suite.as_deref(),
                crate::evals::EvalOptions {
                    verbose,
                    check,
                    streams,
                    sweep,
                    k_sensitivity,
                    compare,
                    embed,
                    pages_from,
                    scope,
                },
                cli.data_dir.clone(),
            )?;
        }
        Commands::Redact { apply } => {
            redact::cmd_redact(apply, cli.data_dir.clone())?;
        }
        Commands::Forget { paths } => {
            cmd_forget(&paths, cli.data_dir.clone())?;
        }
        Commands::ForgetSession { sessions, apply } => {
            cmd_forget_session(&sessions, apply, cli.data_dir.clone())?;
        }
        Commands::Abstracts { suite, write, pace } => {
            cmd_abstracts(&suite, write, std::time::Duration::from_secs(pace))?;
        }
        Commands::Reconsolidate {
            sessions,
            apply,
            show_prompt,
        } => {
            cmd_reconsolidate(&sessions, apply, show_prompt, cli.data_dir.clone())?;
        }
        Commands::Backup { out } => {
            cmd_backup(out, cli.data_dir.clone())?;
        }
        Commands::Restore {
            archive,
            apply,
            force,
        } => {
            cmd_restore(&archive, apply, force, cli.data_dir.clone())?;
        }
        Commands::Audit { limit, everywhere } => {
            cmd_audit(limit, everywhere, cli.data_dir.clone())?;
        }
        Commands::Run {
            agent,
            program,
            server,
            token,
            anyway,
            args,
        } => {
            cmd_run(
                &agent,
                program,
                &args,
                &server,
                token.as_deref(),
                anyway,
                cli.data_dir.clone(),
            )?;
        }
        Commands::Continue {
            program,
            server,
            token,
            anyway,
            args,
        } => {
            cmd_continue(
                program,
                &args,
                &server,
                token.as_deref(),
                anyway,
                cli.data_dir.clone(),
            )?;
        }
        Commands::Bench { events } => {
            cmd_bench(events)?;
        }
        Commands::Purge { apply } => {
            cmd_purge(apply, cli.data_dir.clone())?;
        }
        Commands::Uninstall { apply } => {
            cmd_uninstall(apply, cli.data_dir.clone())?;
        }
        Commands::Rename { name, apply } => {
            cmd_rename(&name, apply, cli.data_dir.clone())?;
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

/// A panic's message and where it happened, as two plain strings for a log line.
fn describe_panic(info: &std::panic::PanicHookInfo<'_>) -> (String, String) {
    let location = info
        .location()
        .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
        .unwrap_or_else(|| "unknown".to_owned());
    (panic_message(info.payload()), location)
}

/// What a panic said, whether it was given a literal or a formatted string.
///
/// `panic!("literal")` carries a `&str` and `panic!("{x}")` a `String`, and a
/// hook that looked for only one of them would log the other as nothing.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic that carried no message".to_owned())
}

fn default_filter(debug: bool) -> &'static str {
    if debug {
        "debug"
    } else {
        "info,refinery_core=warn"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Both shapes a panic's message comes in, and the one that is neither.
    #[test]
    fn a_panic_is_logged_with_what_it_said() {
        let literal: Box<dyn std::any::Any + Send> = Box::new("index out of range");
        let formatted: Box<dyn std::any::Any + Send> = Box::new(format!("page {} missing", 7));
        let other: Box<dyn std::any::Any + Send> = Box::new(42_u32);

        assert_eq!(panic_message(literal.as_ref()), "index out of range");
        assert_eq!(panic_message(formatted.as_ref()), "page 7 missing");
        assert_eq!(
            panic_message(other.as_ref()),
            "a panic that carried no message"
        );
    }
}
