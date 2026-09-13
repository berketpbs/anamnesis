//! `anamnesis service`: keep the server running without a terminal.
//!
//! A server started by hand lives as long as its terminal, and this project's
//! own memory recorded nothing for four days because a window was closed.
//! `GETTING_STARTED.md` answered that with a page of PowerShell per platform,
//! and every line of it was there because something had gone wrong without it:
//! Task Scheduler's own restart does not cover a process that exits non-zero,
//! its default time limit kills a task after three days, a repeating trigger
//! without `IgnoreNew` stacks copies, and an interactive task shows a console
//! window every time it really starts something. Following that page correctly
//! was the installation, and the machine this project is developed on ended up
//! running a VBScript that ran a PowerShell script that ran the server.
//!
//! This writes the same definition from one place, reads it back to check what
//! was registered rather than what was asked for, and starts the server.
//!
//! * **Windows**: a scheduled task for this account — at logon, and every
//!   minute so a dead server comes back, `IgnoreNew` so a live one is left
//!   alone, no time limit. The action is `conhost.exe --headless`, which gives
//!   the server a console that is never drawn: no window, and no elevation,
//!   where the only earlier way to that was a script host. Measured before this
//!   was written: a task running `conhost --headless` showed no window, stayed
//!   `Running` while its child ran and went `Ready` when it exited.
//! * **Linux**: a systemd user unit, `Restart=always`.
//! * **macOS**: a launchd agent, `RunAtLoad` and `KeepAlive`.
//!
//! The server then reads its settings from `settings.env` and its key from the
//! credential store, so nothing about the environment it starts in matters.

use std::path::{Path, PathBuf};
use std::process::Command;

use anamnesis_core::datadir::DataDir;

/// The scheduled task's name, the one the documentation has always used.
pub const TASK_NAME: &str = "Anamnesis Memory Server";

/// The systemd unit's name.
const UNIT_NAME: &str = "anamnesis.service";

/// The launchd agent's label.
const AGENT_LABEL: &str = "dev.anamnesis.server";

/// The port `serve` takes when none is given.
const DEFAULT_PORT: u16 = 8080;

/// Which service manager this machine has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    TaskScheduler,
    Systemd,
    Launchd,
}

impl Manager {
    fn here() -> Option<Self> {
        if cfg!(windows) {
            Some(Self::TaskScheduler)
        } else if cfg!(target_os = "macos") {
            Some(Self::Launchd)
        } else if cfg!(target_os = "linux") {
            Some(Self::Systemd)
        } else {
            None
        }
    }
}

/// What the service runs: a binary and the arguments after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The binary.
    pub binary: PathBuf,
    /// Everything after it, `serve` included.
    pub args: Vec<String>,
}

impl Launch {
    /// `serve`, with the data directory and the port when they are not the
    /// defaults — a service starts with none of the environment that chose
    /// them.
    fn new(binary: PathBuf, data_dir: Option<&Path>, port: u16) -> Self {
        let mut args = Vec::new();
        if let Some(dir) = data_dir {
            args.push("--data-dir".to_owned());
            args.push(dir.display().to_string());
        }
        args.push("serve".to_owned());
        if port != DEFAULT_PORT {
            args.push("--port".to_owned());
            args.push(port.to_string());
        }
        Self { binary, args }
    }

    /// The whole command line, quoted for Windows' `CreateProcess`.
    fn windows_command_line(&self) -> String {
        std::iter::once(self.binary.display().to_string())
            .chain(self.args.iter().cloned())
            .map(|part| quote_windows(&part))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Quote one argument the way `CreateProcess` splits them, when it needs it.
fn quote_windows(part: &str) -> String {
    if !part.is_empty() && !part.contains([' ', '\t', '"']) {
        return part.to_owned();
    }
    format!("\"{}\"", part.replace('"', "\\\""))
}

/// Escape text for an XML element.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The scheduled task, as the XML `schtasks /Create /XML` takes.
///
/// Every element that matters is written rather than left to a default,
/// because each default is one of the ways this has stopped: `PT0S` or the task
/// is killed after three days, `IgnoreNew` or the repetition stacks servers,
/// both battery settings or a laptop unplugged stops it. The repetition has no
/// `<Duration>`, which is how the schema says "indefinitely" — the obvious
/// maximum value is rejected as out of range.
fn task_xml(user: &str, conhost: &str, launch: &Launch, start: &str) -> String {
    let user = xml_escape(user);
    let conhost = xml_escape(conhost);
    let arguments = xml_escape(&format!("--headless {}", launch.windows_command_line()));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>anamnesis memory server. Written by `anamnesis service install`; remove with `anamnesis service uninstall`.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
    <TimeTrigger>
      <Repetition>
        <Interval>PT1M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{conhost}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// The settings a registered task has to carry, checked in what came back.
///
/// Read back from Task Scheduler rather than trusted from what was sent: a
/// registration that failed leaves the previous definition in place, and the
/// next thing to describe the task describes that one.
fn task_is_sound(registered: &str) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for (needle, what) in [
        ("<MultipleInstancesPolicy>IgnoreNew", "IgnoreNew"),
        ("<ExecutionTimeLimit>PT0S", "no time limit"),
        ("<Interval>PT1M", "the one-minute restart"),
        ("LogonTrigger", "the logon trigger"),
        ("--headless", "the headless console"),
    ] {
        if !registered.contains(needle) {
            missing.push(what);
        }
    }
    missing
}

/// The systemd user unit.
fn systemd_unit(launch: &Launch, data: &DataDir) -> String {
    let command = std::iter::once(launch.binary.display().to_string())
        .chain(launch.args.iter().cloned())
        .map(|part| {
            if part.contains([' ', '"', '\\']) {
                format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let keys = data.root().join("keys.env");
    format!(
        "# Written by `anamnesis service install`; remove with `anamnesis service uninstall`.\n\
         [Unit]\n\
         Description=anamnesis memory server\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={command}\n\
         Restart=always\n\
         RestartSec=5\n\
         # Model keys, since Linux has no credential store anamnesis uses. Optional\n\
         # (the leading -), and keep it readable only by you: chmod 600.\n\
         EnvironmentFile=-{keys}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        keys = keys.display()
    )
}

/// The launchd agent.
fn launchd_plist(launch: &Launch, data: &DataDir) -> String {
    let arguments = std::iter::once(launch.binary.display().to_string())
        .chain(launch.args.iter().cloned())
        .map(|part| format!("    <string>{}</string>", xml_escape(&part)))
        .collect::<Vec<_>>()
        .join("\n");
    let stderr = data.logs().join("launchd-stderr.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `anamnesis service install`; remove with `anamnesis service uninstall`. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{arguments}
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#,
        stderr = xml_escape(&stderr.display().to_string())
    )
}

/// Whether a binary lives in a cargo build directory.
///
/// A service pointed there runs whatever the last build left, and on Windows
/// holds the file open so the next `cargo build` cannot replace it — which is
/// how the first server on the machine this was written on stopped a build.
fn in_build_dir(binary: &Path) -> bool {
    // Split on both separators rather than by `Path::components`, which only
    // knows the platform's own: on Linux a Windows path is one component, and
    // the check passed on Windows and failed in CI.
    let parts: Vec<String> = binary
        .to_string_lossy()
        .split(['/', '\\'])
        .map(str::to_ascii_lowercase)
        .collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == "target" && (pair[1] == "debug" || pair[1] == "release"))
}

/// Run a program and return its standard output, or say what it said instead.
fn run(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| anyhow::anyhow!("could not run {program}: {error}"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = if said.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout)
        } else {
            said
        };
        anyhow::bail!("`{program} {}` failed: {}", args.join(" "), said.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether a server answers on this machine at `port`.
fn server_answers(port: u16) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|client| {
            client
                .get(format!("http://127.0.0.1:{port}/health"))
                .send()
                .ok()
        })
        .is_some_and(|response| response.status().is_success())
}

/// What the server will start with, said before it is started.
///
/// The failure this is written against is a service that runs perfectly and
/// summarises every session by counting, because the environment that had the
/// model in it was a terminal's.
fn describe_readiness(launch: &Launch, data: &DataDir) {
    println!("  Binary:    {}", launch.binary.display());
    let settings = crate::settings::read(&data.root().join(crate::settings::FILE));
    println!(
        "  Settings:  {}{}",
        settings.path.display(),
        if settings.present { "" } else { " (none)" }
    );

    // What the service will see: the file and the store, and not this shell.
    // Reading the shell here would report the model a terminal has, which is
    // exactly the one a service started at login does not.
    let service_var = |name: &str| {
        settings.values.get(name).cloned().or_else(|| {
            crate::keys::KEY_NAMES
                .contains(&name)
                .then(|| crate::keys::stored(name))
                .flatten()
        })
    };
    let shell_only: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| {
            (name.starts_with("ANAMNESIS_LLM_") || name.starts_with("ANAMNESIS_EMBED_"))
                && !settings.values.contains_key(name)
                && !crate::settings::SECRET_NAMES.contains(&name.as_str())
        })
        .collect();

    match anamnesis_llm::LlmConfig::from_vars(service_var) {
        Ok(llm) if llm.provider == anamnesis_llm::ProviderKind::None => println!(
            "  Model:     none — every session will be summarised by counting. Set \
             ANAMNESIS_LLM_PROVIDER in settings.env and the key with `anamnesis key set`."
        ),
        Ok(llm) => println!("  Model:     {} ({:?})", llm.model, llm.provider),
        Err(error) => println!("  Model:     misconfigured — {error}"),
    }
    let embed = anamnesis_llm::EmbedConfig::from_vars(service_var);
    if embed.enabled {
        println!("  Vectors:   {}", embed.model);
    } else {
        println!("  Vectors:   off");
    }
    if !shell_only.is_empty() {
        println!(
            "  ⚠ Set in this shell but not in settings.env, so the service will not see them: {}",
            shell_only.join(", ")
        );
    }
}

/// `anamnesis service install`.
pub fn cmd_service_install(
    write: bool,
    binary: Option<PathBuf>,
    port: u16,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let Some(manager) = Manager::here() else {
        anyhow::bail!("no service manager anamnesis knows on this platform");
    };
    let data = DataDir::resolve(data_dir.clone())?;
    let named = binary.is_some();
    let binary = match binary {
        Some(binary) => binary,
        None => std::env::current_exe()?,
    };
    let launch = Launch::new(binary, data_dir.as_deref(), port);

    println!("🛎  anamnesis service");
    println!();
    describe_readiness(&launch, &data);
    println!();

    if in_build_dir(&launch.binary) {
        let advice = "copy it somewhere stable — beside the data directory, say — and run \
                      that copy's `service install`, or pass --binary";
        if write && !named {
            anyhow::bail!(
                "{} is in a cargo build directory: the service would run whatever the next \
                 build leaves, and on Windows keep that build from replacing it. {advice}",
                launch.binary.display()
            );
        }
        println!(
            "  ⚠ {} is in a cargo build directory; {advice}.",
            launch.binary.display()
        );
        println!();
    }

    match manager {
        Manager::TaskScheduler => install_task(&launch, port, write),
        Manager::Systemd => install_unit(&launch, &data, write),
        Manager::Launchd => install_agent(&launch, &data, write),
    }
}

fn install_task(launch: &Launch, port: u16, write: bool) -> anyhow::Result<()> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (_, Ok(name)) => name,
        _ => anyhow::bail!("USERNAME is not set, so there is no account to register the task for"),
    };
    let start = jiff::Zoned::now().strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let conhost = format!(
        r"{}\System32\conhost.exe",
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned())
    );
    let xml = task_xml(&user, &conhost, launch, &start);

    if !write {
        println!("Would register the scheduled task {TASK_NAME:?} for {user}:");
        println!();
        println!("{xml}");
        println!("Run this again with --write to register it and start the server.");
        return Ok(());
    }

    // schtasks reads a task file as UTF-16, which is what its declaration says.
    let mut file = tempfile::Builder::new()
        .prefix("anamnesis-task-")
        .suffix(".xml")
        .tempfile()?;
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::io::Write::write_all(&mut file, &bytes)?;
    // Closed before schtasks opens it, and still deleted when this returns.
    // Held open, Windows refuses the second reader: the first real run of this
    // failed with "the process cannot access the file because it is being used
    // by another process".
    let file = file.into_temp_path();
    let path = file.display().to_string();

    run(
        "schtasks",
        &["/Create", "/TN", TASK_NAME, "/XML", &path, "/F"],
    )?;
    let registered = run("schtasks", &["/Query", "/TN", TASK_NAME, "/XML"])?;
    let missing = task_is_sound(&registered);
    if !missing.is_empty() {
        anyhow::bail!(
            "the task Task Scheduler holds is missing {}; it is not the one this wrote",
            missing.join(", ")
        );
    }
    println!("🛎  Registered {TASK_NAME:?} for {user}, and read it back.");
    start_or_explain(port, || {
        run("schtasks", &["/Run", "/TN", TASK_NAME]).map(|_| ())
    })
}

fn install_unit(launch: &Launch, data: &DataDir, write: bool) -> anyhow::Result<()> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("no user configuration directory"))?
        .join("systemd")
        .join("user");
    let path = dir.join(UNIT_NAME);
    let unit = systemd_unit(launch, data);

    if !write {
        println!("Would write {}:", path.display());
        println!();
        println!("{unit}");
        println!(
            "and run `systemctl --user daemon-reload` and `systemctl --user enable --now {UNIT_NAME}`."
        );
        println!("Run this again with --write to do it.");
        return Ok(());
    }

    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, unit)?;
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", "--now", UNIT_NAME])?;
    println!("🛎  Wrote {} and started it.", path.display());
    println!();
    println!("  A user unit stops when you log out unless lingering is on:");
    println!("  loginctl enable-linger $USER");
    Ok(())
}

fn install_agent(launch: &Launch, data: &DataDir, write: bool) -> anyhow::Result<()> {
    let path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?
        .join("Library/LaunchAgents")
        .join(format!("{AGENT_LABEL}.plist"));
    let plist = launchd_plist(launch, data);
    let uid = run("id", &["-u"])?.trim().to_owned();
    let domain = format!("gui/{uid}");

    if !write {
        println!("Would write {}:", path.display());
        println!();
        println!("{plist}");
        println!("and run `launchctl bootstrap {domain} {}`.", path.display());
        println!("Run this again with --write to do it.");
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(data.logs())?;
    // Out first, so installing twice replaces the agent rather than failing on
    // the one already loaded. Not being loaded is not an error here.
    let _ = run(
        "launchctl",
        &["bootout", &domain, &path.display().to_string()],
    );
    std::fs::write(&path, plist)?;
    run(
        "launchctl",
        &["bootstrap", &domain, &path.display().to_string()],
    )?;
    println!("🛎  Wrote {} and loaded it.", path.display());
    Ok(())
}

/// Start the server through the service, unless one already answers.
///
/// A server somebody started by hand holds the port, so a task started now
/// would fail to bind and try again every minute. That is said, rather than
/// done.
fn start_or_explain(port: u16, start: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    println!();
    if server_answers(port) {
        println!("  A server already answers on port {port}. The service cannot take the port");
        println!("  while it runs: stop that one, and the service starts its own within a minute.");
        return Ok(());
    }
    start()?;
    for _ in 0..30 {
        if server_answers(port) {
            println!("  Started, and answering on port {port}.");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!("  Started, but nothing answered on port {port} within 15 seconds.");
    println!("  The server's own account of why is in the data directory's logs/.");
    Ok(())
}

/// `anamnesis service uninstall`.
pub fn cmd_service_uninstall(write: bool) -> anyhow::Result<()> {
    let Some(manager) = Manager::here() else {
        anyhow::bail!("no service manager anamnesis knows on this platform");
    };
    match manager {
        Manager::TaskScheduler => {
            if !write {
                println!(
                    "Would delete the scheduled task {TASK_NAME:?}. Run with --write to do it."
                );
                return Ok(());
            }
            run("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"])?;
            println!("🛎  Deleted {TASK_NAME:?}.");
            println!("  A server it started keeps running until it is stopped or you log out:");
            println!("  deleting a task does not end the process it launched.");
        }
        Manager::Systemd => {
            let path = dirs::config_dir()
                .ok_or_else(|| anyhow::anyhow!("no user configuration directory"))?
                .join("systemd/user")
                .join(UNIT_NAME);
            if !write {
                println!(
                    "Would stop and disable {UNIT_NAME} and remove {}. Run with --write.",
                    path.display()
                );
                return Ok(());
            }
            run("systemctl", &["--user", "disable", "--now", UNIT_NAME])?;
            std::fs::remove_file(&path)?;
            run("systemctl", &["--user", "daemon-reload"])?;
            println!("🛎  Stopped and removed {UNIT_NAME}.");
        }
        Manager::Launchd => {
            let path = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("no home directory"))?
                .join("Library/LaunchAgents")
                .join(format!("{AGENT_LABEL}.plist"));
            if !write {
                println!(
                    "Would unload and remove {}. Run with --write.",
                    path.display()
                );
                return Ok(());
            }
            let uid = run("id", &["-u"])?.trim().to_owned();
            run(
                "launchctl",
                &[
                    "bootout",
                    &format!("gui/{uid}"),
                    &path.display().to_string(),
                ],
            )?;
            std::fs::remove_file(&path)?;
            println!("🛎  Unloaded and removed {}.", path.display());
        }
    }
    Ok(())
}

/// `anamnesis service status`: whether a service is registered, what it runs,
/// and whether a server answers.
pub fn cmd_service_status(port: u16, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let Some(manager) = Manager::here() else {
        anyhow::bail!("no service manager anamnesis knows on this platform");
    };
    let data = DataDir::resolve(data_dir)?;
    println!("🛎  anamnesis service");
    println!();
    match manager {
        Manager::TaskScheduler => match run("schtasks", &["/Query", "/TN", TASK_NAME, "/XML"]) {
            Ok(registered) => {
                let action =
                    between(&registered, "<Arguments>", "</Arguments>").unwrap_or_default();
                println!("  Task:      {TASK_NAME:?}");
                println!("  Runs:      {}", unescape_xml(action));
                let missing = task_is_sound(&registered);
                if missing.is_empty() {
                    println!(
                        "  Settings:  logon and one-minute restart, IgnoreNew, no time limit, headless"
                    );
                } else {
                    println!(
                        "  Settings:  missing {} — not written by `service install`; run it with --write",
                        missing.join(", ")
                    );
                }
            }
            Err(_) => println!("  Task:      not registered — `anamnesis service install --write`"),
        },
        Manager::Systemd => {
            let active = run("systemctl", &["--user", "is-active", UNIT_NAME])
                .map(|out| out.trim().to_owned())
                .unwrap_or_else(|_| "not active".to_owned());
            println!("  Unit:      {UNIT_NAME} ({active})");
        }
        Manager::Launchd => {
            let loaded = run("launchctl", &["list", AGENT_LABEL]).is_ok();
            println!(
                "  Agent:     {AGENT_LABEL} ({})",
                if loaded { "loaded" } else { "not loaded" }
            );
        }
    }
    println!(
        "  Server:    {}",
        if server_answers(port) {
            format!("answering on port {port}")
        } else {
            format!("nothing answers on port {port}")
        }
    );
    println!("  Logs:      {}", data.logs().display());
    Ok(())
}

/// The text between two markers, the first time they appear.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

fn unescape_xml(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONHOST: &str = r"C:\Windows\System32\conhost.exe";

    fn launch() -> Launch {
        Launch::new(
            PathBuf::from(r"C:\Users\A Person\AppData\Roaming\anamnesis\bin\anamnesis.exe"),
            None,
            DEFAULT_PORT,
        )
    }

    /// Each of these was a way the documented task stopped, or would have.
    #[test]
    fn the_task_carries_every_setting_that_keeps_it_running() {
        let xml = task_xml(r"VICTUS\Berke", CONHOST, &launch(), "2026-09-13T21:00:00");

        assert!(task_is_sound(&xml).is_empty(), "{:?}", task_is_sound(&xml));
        assert!(
            xml.contains("<LogonType>InteractiveToken</LogonType>"),
            "no elevation needed"
        );
        assert!(xml.contains("<StopIfGoingOnBatteries>false"));
        assert!(
            !xml.contains("<Duration>"),
            "an absent duration is indefinite"
        );
        assert!(
            xml.contains(
                "<Arguments>--headless &quot;C:\\Users\\A Person\\AppData\\Roaming\\anamnesis\\bin\\anamnesis.exe&quot; serve</Arguments>"
            ),
            "the path with a space is quoted, and escaped for XML: {xml}"
        );
        assert_eq!(xml.matches(r"<UserId>VICTUS\Berke</UserId>").count(), 2);
    }

    #[test]
    fn a_task_missing_a_setting_is_named_by_what_it_is_missing() {
        let edited = task_xml("u", CONHOST, &launch(), "2026-09-13T21:00:00")
            .replace("IgnoreNew", "Parallel")
            .replace("PT0S", "PT72H");
        assert_eq!(task_is_sound(&edited), ["IgnoreNew", "no time limit"]);
    }

    /// A service starts with none of the environment that chose these.
    #[test]
    fn a_data_directory_and_port_that_are_not_defaults_are_carried() {
        let custom = Launch::new(
            PathBuf::from("/usr/local/bin/anamnesis"),
            Some(Path::new("/srv/memory")),
            8099,
        );
        assert_eq!(
            custom.args,
            ["--data-dir", "/srv/memory", "serve", "--port", "8099"]
        );
        assert_eq!(
            launch().args,
            ["serve"],
            "defaults are left to the defaults"
        );
    }

    #[test]
    fn the_unit_restarts_always_and_takes_keys_from_a_file_it_does_not_require() {
        let launch = Launch::new(
            PathBuf::from("/home/me/.local/bin/anamnesis"),
            None,
            DEFAULT_PORT,
        );
        let unit = systemd_unit(&launch, &DataDir::new("/home/me/.local/share/anamnesis"));

        assert!(
            unit.contains("ExecStart=/home/me/.local/bin/anamnesis serve\n"),
            "{unit}"
        );
        assert!(unit.contains("Restart=always"));
        assert!(
            unit.contains("EnvironmentFile=-/home/me/.local/share/anamnesis"),
            "{unit}"
        );
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn the_agent_runs_at_load_and_is_kept_alive() {
        let launch = Launch::new(PathBuf::from("/Users/me/bin/anamnesis"), None, DEFAULT_PORT);
        let plist = launchd_plist(
            &launch,
            &DataDir::new("/Users/me/Library/Application Support/anamnesis"),
        );

        assert!(
            plist.contains("<string>/Users/me/bin/anamnesis</string>\n    <string>serve</string>"),
            "{plist}"
        );
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains(AGENT_LABEL));
    }

    #[test]
    fn a_binary_in_a_cargo_build_directory_is_recognised() {
        assert!(in_build_dir(Path::new(
            r"C:\Berke\anamnesis\target\release\anamnesis.exe"
        )));
        assert!(in_build_dir(Path::new(
            "/src/anamnesis/target/debug/anamnesis"
        )));
        assert!(!in_build_dir(Path::new(
            r"C:\Users\me\AppData\Roaming\anamnesis\bin\anamnesis.exe"
        )));
        assert!(
            !in_build_dir(Path::new("/opt/target/anamnesis")),
            "a directory merely named target"
        );
    }

    #[test]
    fn arguments_are_quoted_only_when_they_need_it() {
        assert_eq!(quote_windows("serve"), "serve");
        assert_eq!(quote_windows(r"C:\a b\x.exe"), r#""C:\a b\x.exe""#);
        assert_eq!(quote_windows(""), r#""""#);
    }

    #[test]
    fn what_a_task_runs_is_read_back_out_of_its_xml() {
        let xml = task_xml("u", CONHOST, &launch(), "2026-09-13T21:00:00");
        let action = between(&xml, "<Arguments>", "</Arguments>").expect("arguments");
        assert!(unescape_xml(action).starts_with("--headless \"C:\\Users\\A Person"));
    }
}
