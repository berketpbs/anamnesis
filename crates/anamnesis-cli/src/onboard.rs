//! `anamnesis setup`: wiring a project, in one command.
//!
//! Getting memory to work took five commands, in an order the documentation
//! gave over several hundred lines — `init`, `install-hooks --write`,
//! `install-mcp --write`, `service install --write`, `bootstrap` — and every
//! one of them had a way to be done wrongly that looked like being done: hooks
//! registered and never firing, an MCP server nobody registered for four
//! months, a server that stopped when its terminal closed. Each of those was
//! found on the machine this project is developed on, by the person who wrote
//! it, days later.
//!
//! So this does not add a sixth way to wire things. It asks each of the five
//! whether its part is already done, prints one line per part, and with
//! `--write` runs the ones that are not — the same commands, with their own
//! reports. Then, if a server answers, it probes the path an event takes, so
//! the last thing it says is whether the next session will be recorded rather
//! than whether files were written.
//!
//! Nothing here is a second copy of what those commands decide. A step is
//! "done" when the command it stands for would change nothing, asked with the
//! same inputs that command uses.

use std::path::{Path, PathBuf};

use anamnesis_core::datadir::DataDir;
use anamnesis_core::scope::{ResolvedScope, resolve_scope};
use anamnesis_store::Store;

use crate::{bootstrap, capture, hooks, mcp_config, opencode, service, setup};

/// What `setup` was asked to do.
pub struct Options {
    /// Harnesses to wire, in the order given.
    pub agents: Vec<String>,
    /// Where hooks deliver. Follows `port` unless given.
    pub server: Option<String>,
    /// Port the service's server listens on.
    pub port: u16,
    /// Leave the service alone.
    pub no_service: bool,
    /// Leave an empty memory empty.
    pub no_seed: bool,
    /// Do it, rather than say what would be done.
    pub write: bool,
}

/// Where one part of the wiring stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Already as it should be.
    Done(String),
    /// Not yet; `--write` does it.
    Todo(String),
    /// Deliberately not done, and why.
    Skipped(String),
    /// Cannot be done by this command, and what to do instead.
    Blocked(String),
}

impl State {
    fn mark(&self) -> &'static str {
        match self {
            State::Done(_) => "✓",
            State::Todo(_) => "→",
            State::Skipped(_) => "·",
            State::Blocked(_) => "✗",
        }
    }

    fn text(&self) -> &str {
        match self {
            State::Done(text) | State::Todo(text) | State::Skipped(text) | State::Blocked(text) => {
                text
            }
        }
    }
}

/// What carrying a step out means.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Init,
    Hooks(String),
    Mcp(String),
    Service,
    Seed,
}

/// One part of the wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    name: &'static str,
    state: State,
    action: Action,
}

/// `anamnesis setup`.
pub fn cmd_setup(options: Options, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir.clone())?;
    let binary = std::env::current_exe()?;
    let server = options
        .server
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", options.port));

    println!("🧭 anamnesis setup — {}", scope.scope);
    println!("   {}", scope.root.display());
    println!();
    service::describe_readiness(
        &service::Launch::new(binary.clone(), data_dir.as_deref(), options.port),
        &data,
    );
    println!();

    // Every file this writes names the binary running it. One in a cargo build
    // directory is replaced by the next build — and on Windows cannot be, while
    // a server or a hook holds it.
    if service::in_build_dir(&binary) {
        let advice = format!(
            "{} is in a cargo build directory, and everything this wires would run whatever the \
             next build leaves there. Copy it somewhere it will stay and run that copy's `setup`.",
            binary.display()
        );
        if options.write {
            anyhow::bail!(advice);
        }
        println!("  ⚠ {advice}");
        println!();
    }

    let steps = plan(
        &options,
        &scope,
        &data,
        data_dir.as_deref(),
        &binary,
        &server,
    );
    for step in &steps {
        println!(
            "  {} {:<7} {}",
            step.state.mark(),
            step.name,
            step.state.text()
        );
    }
    println!();

    let todo: Vec<&Step> = steps
        .iter()
        .filter(|step| matches!(step.state, State::Todo(_)))
        .collect();
    let blocked = steps
        .iter()
        .filter(|step| matches!(step.state, State::Blocked(_)))
        .count();

    if !options.write {
        match todo.len() {
            0 if blocked == 0 => println!("  Nothing to do: this project is wired."),
            0 => println!("  Nothing `--write` can do; see the lines marked ✗."),
            n => println!(
                "  {n} step(s) to do. Run `anamnesis setup --write` to do {}.",
                if n == 1 { "it" } else { "them" }
            ),
        }
        return Ok(());
    }

    for step in &todo {
        println!("── {} ──", step.name);
        println!();
        // The files are named from the project root rather than left to the
        // commands' default of the working directory, so `setup` run from a
        // subdirectory wires the project and not the subdirectory — and writes
        // exactly the files its plan named.
        match &step.action {
            Action::Init => setup::cmd_init(data_dir.clone())?,
            Action::Hooks(agent) => {
                let settings = if agent == "opencode" {
                    Some(opencode::plugin_path(&scope.root))
                } else {
                    hooks::harness(agent)
                        .map(|harness| hooks::default_settings_path(&harness, &scope.root))
                };
                setup::cmd_install_hooks(agent, &server, true, settings)?
            }
            Action::Mcp(agent) => setup::cmd_install_mcp(
                agent,
                true,
                mcp_config::target(agent)
                    .map(|target| mcp_config::config_path(&target, &scope.root)),
                Some(scope.root.clone()),
                data_dir.clone(),
            )?,
            Action::Service => {
                service::cmd_service_install(true, None, options.port, data_dir.clone())?
            }
            Action::Seed => bootstrap::cmd_bootstrap(
                &scope.root,
                false,
                bootstrap::DEFAULT_MAX_COMMITS,
                false,
                data_dir.clone(),
            )?,
        }
        println!();
    }

    // The files are the means. What the person wants to know is whether the
    // next session will be recorded, and that is asked of the server, with a
    // probe that records nothing.
    let wired = options
        .agents
        .iter()
        .find(|agent| hooks::harness(agent).is_some() || agent.as_str() == "opencode");
    if let Some(agent) = wired
        && service::server_answers(options.port)
    {
        println!("── check ──");
        println!();
        let token = std::env::var(anamnesis_web::auth::TOKEN_ENV).ok();
        capture::probe(
            agent,
            &server,
            token.as_deref(),
            capture::made_up_payload(agent)?,
        )?;
        println!();
    }

    if blocked > 0 {
        anyhow::bail!(
            "{blocked} step(s) could not be done by setup; the lines marked ✗ above say what to do"
        );
    }
    if todo
        .iter()
        .any(|step| matches!(step.action, Action::Hooks(_) | Action::Mcp(_)))
    {
        println!("  Hooks and MCP servers are read when a session starts: the next session");
        println!("  opened here is the first one recorded, not the one running now.");
    } else if todo.is_empty() {
        println!("  Nothing to do: this project is wired.");
    }
    Ok(())
}

/// Where every part of the wiring stands, asked the way each command would.
fn plan(
    options: &Options,
    scope: &ResolvedScope,
    data: &DataDir,
    data_dir: Option<&Path>,
    binary: &Path,
    server: &str,
) -> Vec<Step> {
    let store = data
        .db_file()
        .exists()
        .then(|| Store::open(data.db_file()).ok())
        .flatten()
        .filter(|store| store.migrate().is_ok());

    let mut steps = vec![memory_step(store.as_ref(), scope, data)];
    let command_binary = binary.display().to_string();
    for agent in &options.agents {
        steps.push(hooks_step(agent, &scope.root, &command_binary, server));
    }
    let (env, _) = setup::mcp_environment(data_dir);
    for agent in &options.agents {
        steps.push(mcp_step(agent, &scope.root, binary, &env));
    }
    steps.push(server_step(
        service::server_answers(options.port),
        service::registration(
            &service::Launch::new(binary.to_path_buf(), data_dir, options.port),
            data,
        ),
        !options.no_service,
        options.port,
    ));
    steps.push(seed_step(
        git2::Repository::discover(&scope.root).is_ok(),
        store
            .as_ref()
            .and_then(|store| store.page_count(scope.project_id).ok()),
        !options.no_seed,
    ));
    steps
}

/// The data directory and this project's row in it.
fn memory_step(store: Option<&Store>, scope: &ResolvedScope, data: &DataDir) -> Step {
    let registered = store
        .and_then(|store| store.projects().ok())
        .is_some_and(|projects| {
            projects
                .iter()
                .any(|project| project.project_id == scope.project_id)
        });
    let state = if registered {
        State::Done(format!(
            "{} is registered in {}",
            scope.scope,
            data.root().display()
        ))
    } else {
        State::Todo(format!(
            "register {} in {}",
            scope.scope,
            data.root().display()
        ))
    };
    Step {
        name: "memory",
        state,
        action: Action::Init,
    }
}

/// One harness's hooks: whether `install-hooks --write` would change its file.
fn hooks_step(agent: &str, root: &Path, binary: &str, server: &str) -> Step {
    let step = |state| Step {
        name: "hooks",
        state,
        action: Action::Hooks(agent.to_owned()),
    };

    if agent == "opencode" {
        let path = opencode::plugin_path(root);
        let shown = shown(&path, root);
        let source = opencode::plugin(binary, server);
        return step(match std::fs::read_to_string(&path) {
            Ok(existing) if existing == source => {
                State::Done(format!("opencode: plugin in {shown}"))
            }
            Ok(_) if !opencode::is_ours(&path) => State::Blocked(format!(
                "opencode: {shown} is a plugin anamnesis did not write; move it aside first"
            )),
            Ok(_) => State::Todo(format!(
                "opencode: rewrite the plugin in {shown} for this binary and server"
            )),
            Err(_) => State::Todo(format!("opencode: write the plugin to {shown}")),
        });
    }

    let Some(harness) = hooks::harness(agent) else {
        return step(State::Blocked(format!(
            "{agent}: no hook template; wired today are {}, opencode",
            hooks::HARNESSES
                .iter()
                .map(|harness| harness.agent)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };

    let path = hooks::default_settings_path(&harness, root);
    let shown = shown(&path, root);
    let config = hooks::hook_config(&harness, &hooks::hook_command(binary, agent, server));
    step(match hooks::read_settings(&path) {
        Err(error) => State::Blocked(format!(
            "{agent}: {shown} could not be read ({error}); fix it or run `install-hooks` to print the lines"
        )),
        Ok(mut settings) => {
            let outcome = hooks::merge(&mut settings, &config);
            if outcome.replaced.is_empty() && outcome.added.is_empty() {
                State::Done(format!(
                    "{agent}: {} event(s) deliver to {server}, in {shown}",
                    outcome.present.len()
                ))
            } else if outcome.present.is_empty() && outcome.replaced.is_empty() {
                State::Todo(format!(
                    "{agent}: wire {} event(s) to {server}, in {shown}",
                    outcome.added.len()
                ))
            } else {
                State::Todo(format!(
                    "{agent}: {} event(s) point somewhere else or are missing, in {shown}",
                    outcome.added.len() + outcome.replaced.len()
                ))
            }
        }
    })
}

/// One harness's MCP registration: whether `install-mcp --write` would change
/// its file.
fn mcp_step(agent: &str, root: &Path, binary: &Path, env: &[(String, String)]) -> Step {
    let step = |state| Step {
        name: "mcp",
        state,
        action: Action::Mcp(agent.to_owned()),
    };

    let Some(target) = mcp_config::target(agent) else {
        return step(State::Skipped(match mcp_config::cannot_register(agent) {
            Some(_) => {
                format!("{agent}: registered by hand — `install-mcp --agent {agent}` says how")
            }
            None => format!("{agent}: no MCP template"),
        }));
    };

    let path = mcp_config::config_path(&target, root);
    let shown = shown(&path, root);
    let entry = mcp_config::server_entry(binary, root, env);
    let registration = match target.format {
        mcp_config::Format::Json => hooks::read_settings(&path)
            .map(|mut config| mcp_config::register(&mut config, mcp_config::SERVER_NAME, &entry)),
        mcp_config::Format::Toml => mcp_config::read_toml(&path).map(|mut document| {
            mcp_config::register_toml(&mut document, mcp_config::SERVER_NAME, &entry)
        }),
    };
    step(match registration {
        Err(error) => State::Blocked(format!(
            "{agent}: {shown} could not be read ({error}); fix it or run `install-mcp` to print the entry"
        )),
        Ok(mcp_config::Registration::Unchanged) => {
            State::Done(format!("{agent}: registered in {shown}"))
        }
        Ok(mcp_config::Registration::Added) => {
            State::Todo(format!("{agent}: register the memory tools in {shown}"))
        }
        Ok(mcp_config::Registration::Replaced(_)) => State::Todo(format!(
            "{agent}: the registration in {shown} starts something else; replace it"
        )),
    })
}

/// The server hooks deliver to, and what keeps it running.
///
/// `registered` is `None` where this platform has no service manager anamnesis
/// knows.
fn server_step(
    answers: bool,
    registered: Option<service::Registered>,
    want_service: bool,
    port: u16,
) -> Step {
    use service::Registered;

    let state = match (answers, registered, want_service) {
        (true, Some(Registered::Same), _) => State::Done(format!(
            "answering on port {port}, kept running by the service"
        )),
        // Something answers, so capture works. Re-registering now would start
        // a second server against the port the first one holds, so it is said
        // rather than done.
        (true, Some(Registered::Different(runs)), _) => State::Done(format!(
            "answering on port {port}; the service runs `{runs}`, not this binary — \
             `anamnesis service install --write` from here changes that"
        )),
        (true, _, _) => State::Done(format!(
            "answering on port {port}, started by hand — it stops with its terminal; \
             `anamnesis service install --write` keeps one running"
        )),
        (false, _, false) => State::Skipped(format!(
            "nothing answers on port {port}; start one with `anamnesis serve`"
        )),
        (false, None, true) => State::Blocked(format!(
            "nothing answers on port {port}, and there is no service manager here anamnesis \
             knows; start one with `anamnesis serve`"
        )),
        (false, Some(Registered::Same), true) => State::Todo(format!(
            "the service is registered and nothing answers on port {port}; register it again \
             and start it (`anamnesis service status` says what its log last said)"
        )),
        (false, Some(Registered::Different(runs)), true) => State::Todo(format!(
            "nothing answers on port {port}, and the service runs `{runs}`; register this \
             binary and start it"
        )),
        (false, Some(Registered::Absent), true) => State::Todo(format!(
            "nothing answers on port {port}; register the service and start it"
        )),
    };
    Step {
        name: "server",
        state,
        action: Action::Service,
    }
}

/// Seeding an empty memory from the repository's history.
fn seed_step(is_git: bool, pages: Option<i64>, want_seed: bool) -> Step {
    let state = match (want_seed, is_git, pages.unwrap_or(0)) {
        (false, _, _) => State::Skipped("left empty, as asked".to_owned()),
        (true, false, _) => {
            State::Skipped("not a git repository; memory fills in from sessions".to_owned())
        }
        (true, true, 0) => {
            State::Todo("memory is empty; seed bootstrap/ pages from git history".to_owned())
        }
        (true, true, pages) => State::Done(format!(
            "memory holds {pages} page(s); bootstrap is for an empty one"
        )),
    };
    Step {
        name: "seed",
        state,
        action: Action::Seed,
    }
}

/// A path as a person in the project reads it.
fn shown(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINARY: &str = "C:/tools/anamnesis.exe";
    const SERVER: &str = "http://127.0.0.1:8080";

    /// A project with nothing in it is four things to do, and the line for
    /// each names the file it will touch.
    #[test]
    fn an_unwired_project_has_its_hooks_and_mcp_to_do() {
        let root = tempfile::tempdir().expect("root");

        let hooks = hooks_step("claude-code", root.path(), BINARY, SERVER);
        assert!(matches!(hooks.state, State::Todo(_)), "{hooks:?}");
        assert!(
            hooks.state.text().contains(".claude"),
            "{}",
            hooks.state.text()
        );

        let mcp = mcp_step("claude-code", root.path(), Path::new(BINARY), &[]);
        assert!(matches!(mcp.state, State::Todo(_)), "{mcp:?}");
        assert!(
            mcp.state.text().contains(".mcp.json"),
            "{}",
            mcp.state.text()
        );
    }

    /// "Done" means the command would change nothing: a file holding exactly
    /// what `install-hooks --write` merges reads as done, and the same file
    /// pointing at another binary does not.
    #[test]
    fn hooks_are_done_only_when_they_run_this_binary() {
        let root = tempfile::tempdir().expect("root");
        let harness = hooks::harness("claude-code").expect("harness");
        let path = hooks::default_settings_path(&harness, root.path());

        let ours = hooks::hook_config(
            &harness,
            &hooks::hook_command(BINARY, "claude-code", SERVER),
        );
        hooks::write_settings(&path, &ours).expect("write");
        let done = hooks_step("claude-code", root.path(), BINARY, SERVER);
        assert!(matches!(done.state, State::Done(_)), "{done:?}");

        let elsewhere = hooks_step("claude-code", root.path(), "D:/old/anamnesis.exe", SERVER);
        assert!(matches!(elsewhere.state, State::Todo(_)), "{elsewhere:?}");
        assert!(
            elsewhere.state.text().contains("point somewhere else"),
            "{}",
            elsewhere.state.text()
        );
    }

    /// A settings file that does not parse is somebody's, and the step that
    /// would write into it is blocked rather than planned.
    #[test]
    fn a_settings_file_that_does_not_parse_blocks_its_step() {
        let root = tempfile::tempdir().expect("root");
        let harness = hooks::harness("claude-code").expect("harness");
        let path = hooks::default_settings_path(&harness, root.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "{ // a comment\n}").expect("write");

        let step = hooks_step("claude-code", root.path(), BINARY, SERVER);
        assert!(matches!(step.state, State::Blocked(_)), "{step:?}");
    }

    #[test]
    fn an_mcp_registration_already_there_is_done() {
        let root = tempfile::tempdir().expect("root");
        let target = mcp_config::target("claude-code").expect("target");
        let path = mcp_config::config_path(&target, root.path());
        let entry = mcp_config::server_entry(Path::new(BINARY), root.path(), &[]);
        mcp_config::apply(&target, &path, mcp_config::SERVER_NAME, &entry).expect("apply");

        let step = mcp_step("claude-code", root.path(), Path::new(BINARY), &[]);
        assert!(matches!(step.state, State::Done(_)), "{step:?}");
    }

    /// An unknown harness is named as unknown, not planned into a file that
    /// nothing reads.
    #[test]
    fn a_harness_with_no_template_is_not_planned() {
        let root = tempfile::tempdir().expect("root");
        let step = hooks_step("zed", root.path(), BINARY, SERVER);
        assert!(matches!(step.state, State::Blocked(_)), "{step:?}");
        let mcp = mcp_step("zed", root.path(), Path::new(BINARY), &[]);
        assert!(matches!(mcp.state, State::Skipped(_)), "{mcp:?}");
    }

    /// The server step's cases, each a different sentence because each needs
    /// a different thing done.
    #[test]
    fn the_server_step_tells_a_kept_server_from_one_started_by_hand() {
        use service::Registered;
        let other = || {
            Some(Registered::Different(
                "--headless D:/old/anamnesis.exe serve".to_owned(),
            ))
        };

        assert!(matches!(
            server_step(true, Some(Registered::Same), true, 8080).state,
            State::Done(text) if text.contains("kept running")
        ));
        assert!(matches!(
            server_step(true, Some(Registered::Absent), true, 8080).state,
            State::Done(text) if text.contains("started by hand")
        ));
        // The case found while writing this: a task that exists and runs a
        // server on another port was reported as keeping this one running.
        assert!(matches!(
            server_step(true, other(), true, 8096).state,
            State::Done(text) if text.contains("D:/old/anamnesis.exe") && !text.contains("kept running")
        ));
        assert!(matches!(
            server_step(false, Some(Registered::Absent), true, 8080).state,
            State::Todo(_)
        ));
        assert!(matches!(
            server_step(false, other(), true, 8080).state,
            State::Todo(text) if text.contains("D:/old/anamnesis.exe")
        ));
        assert!(matches!(
            server_step(false, Some(Registered::Same), true, 8080).state,
            State::Todo(text) if text.contains("service status")
        ));
        assert!(matches!(
            server_step(false, Some(Registered::Absent), false, 8080).state,
            State::Skipped(_)
        ));
        assert!(matches!(
            server_step(false, None, true, 8080).state,
            State::Blocked(_)
        ));
    }

    /// Bootstrap seeds; it does not maintain. A memory that already has pages
    /// is left to them.
    #[test]
    fn only_an_empty_memory_in_a_repository_is_seeded() {
        assert!(matches!(
            seed_step(true, Some(0), true).state,
            State::Todo(_)
        ));
        assert!(matches!(seed_step(true, None, true).state, State::Todo(_)));
        assert!(matches!(
            seed_step(true, Some(12), true).state,
            State::Done(_)
        ));
        assert!(matches!(
            seed_step(false, Some(0), true).state,
            State::Skipped(_)
        ));
        assert!(matches!(
            seed_step(true, Some(0), false).state,
            State::Skipped(_)
        ));
    }
}
