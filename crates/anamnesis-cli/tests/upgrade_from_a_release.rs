//! A data directory written by a released anamnesis, opened by this one.
//!
//! Upgrading is replacing the binary and starting the server again, and the
//! server migrates whatever index it finds. Every migration has tests of its
//! own, each against the schema just before it, and none of them start from
//! a directory a release actually wrote: its transcripts, its wiki, a note
//! still waiting for the next session. This does. A released binary records a
//! session and a decision; this checkout's binary is started on the same
//! directory, and the next session has to be handed that note, the decision
//! has to be found, and the migrated index has to agree with what a rebuild
//! from the wiki and the transcripts makes of them.
//!
//! The released binaries are downloaded, so this does not run with the other
//! tests. CI names them in `ANAMNESIS_UPGRADE_FROM`, separated the way `PATH`
//! is, and runs `cargo test -p anamnesis-cli --test upgrade_from_a_release --
//! --ignored`.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROMPT: &str = "Make the importer skip rows with an empty amount.";
const DECISION: &str = "decisions/ledger-stays-in-sqlite.md";

/// `binary`, with this machine's anamnesis settings taken out of its
/// environment, so neither version reads a key or a data directory the test
/// did not give it.
fn anamnesis(binary: &Path, data: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(binary);
    for (name, _) in std::env::vars() {
        if name.starts_with("ANAMNESIS_")
            || name.ends_with("_API_KEY")
            || name == "ANTHROPIC_API_KEY"
        {
            command.env_remove(name);
        }
    }
    command.env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-upgrade");
    command.current_dir(cwd);
    command.arg("--data-dir").arg(data);
    command
}

fn run<I, S>(binary: &Path, data: &Path, cwd: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    anamnesis(binary, data, cwd)
        .args(args)
        .output()
        .expect("the command starts")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `binary`'s server on a free port and return its address once it
/// accepts connections.
///
/// The port is chosen here rather than asked of the server with `--port 0`:
/// 1.0.0's banner names the port it was given, not the one it bound, so the
/// banner of the oldest release does not say where it is. It is chosen by
/// binding and letting go, so something else can take it before the server
/// binds it — the port comes from the range outgoing connections use too.
/// A server that exits without listening is started again on another port,
/// three times at most, and what it said on stderr is part of the failure.
fn serve(binary: &Path, data: &Path, cwd: &Path) -> (Server, String) {
    let mut exits = Vec::new();
    for _ in 0..3 {
        match serve_once(binary, data, cwd) {
            Ok(started) => return started,
            Err(exit) => exits.push(exit),
        }
    }
    panic!(
        "{} exited without listening, three times:\n{}",
        binary.display(),
        exits.join("\n---\n")
    );
}

/// One attempt of [`serve`]: `Err` with the exit status and stderr when the
/// server exits before it listens.
fn serve_once(binary: &Path, data: &Path, cwd: &Path) -> Result<(Server, String), String> {
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port();
    let mut child = anamnesis(binary, data, cwd)
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--no-watch",
            "--no-ui",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server starts");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");
    let mut server = Server(child);

    // Both pipes are read to their end on threads of their own, so neither
    // fills up or closes under a server that is still writing to it.
    let (seen, banner) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = Some(seen);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("serving on ")
                && let Some(seen) = seen.take()
            {
                let _ = seen.send(());
            }
        }
    });
    let said = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let collected = std::sync::Arc::clone(&said);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Ok(mut said) = collected.lock() {
                said.push_str(&line);
                said.push('\n');
            }
        }
    });
    let exited = |server: &mut Server| {
        server.0.try_wait().ok().flatten().map(|status| {
            // Give the reader a moment to take what the process left.
            std::thread::sleep(Duration::from_millis(200));
            let said = said.lock().map(|s| s.clone()).unwrap_or_default();
            format!("port {port}, {status}; stderr:\n{said}")
        })
    };

    if banner.recv_timeout(Duration::from_secs(15)).is_err() {
        // Its stdout closed, or nothing came for fifteen seconds. A server
        // that closed its stdout is on its way out but may not have exited
        // yet, so it is given a moment to be seen exiting.
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Some(exit) = exited(&mut server) {
                return Err(exit);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "{} printed no banner; stderr:\n{}",
            binary.display(),
            said.lock().map(|s| s.clone()).unwrap_or_default()
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if let Some(exit) = exited(&mut server) {
            return Err(exit);
        }
        assert!(
            Instant::now() < deadline,
            "{} is still running and not listening on {port}; stderr:\n{}",
            binary.display(),
            said.lock().map(|s| s.clone()).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok((server, format!("http://127.0.0.1:{port}")))
}

/// Run one Claude Code hook with `binary`, and return what it printed.
///
/// A hook gives up on the server after a fraction of a second, sets the event
/// aside, says `could not reach` on stderr and still exits 0, so a session can
/// quietly never arrive. A Windows runner starting a release's server can be
/// that slow, and on main's CI 1.1.0's session once never arrived, with
/// nothing on record about why. A hook that could not reach the server is run
/// again, five times at most, and what it said is part of the failure.
fn hook(binary: &Path, data: &Path, repo: &Path, server: &str, payload: &Value) -> String {
    let mut said = String::new();
    for attempt in 1..=5u64 {
        let mut child = anamnesis(binary, data, repo)
            .args(["hook", "--agent", "claude-code", "--server", server])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the hook starts");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.to_string().as_bytes())
            .expect("write the payload");
        let output = child.wait_with_output().expect("the hook finishes");
        said = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "a hook always exits 0; stderr: {said}"
        );
        if !said.contains("could not reach") {
            return String::from_utf8(output.stdout).expect("stdout is text");
        }
        std::thread::sleep(Duration::from_millis(500 * attempt));
    }
    panic!(
        "{} could not reach {server}, five times: {said}",
        binary.display()
    );
}

fn event(session: &str, name: &str, repo: &Path, extra: Value) -> Value {
    let mut payload = json!({
        "session_id": session,
        "hook_event_name": name,
        "cwd": repo.to_string_lossy(),
    });
    if let (Some(base), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    payload
}

/// What `binary`'s `sessions` lists, once it shows a `closed` session.
fn wait_for_closed(binary: &Path, data: &Path, repo: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let listed = text(&run(binary, data, repo, ["sessions"]));
        if listed.contains(" closed ") || Instant::now() > deadline {
            return listed;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn project(name: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        repo.path().join(".anamnesis.toml"),
        format!("[scope]\nworkspace = \"default\"\nproject = \"{name}\"\n"),
    )
    .expect("marker");
    repo
}

fn upgrade_from(released: &Path) {
    let current = Path::new(env!("CARGO_BIN_EXE_anamnesis"));
    let version = text(&run(
        released,
        Path::new("."),
        Path::new("."),
        ["--version"],
    ));
    let version = version.trim();
    let data = tempfile::tempdir().expect("data dir");
    let repo = project("upgrade");
    let (data, repo) = (data.path(), repo.path());

    // What the release leaves: a finished session with its note waiting, and
    // a decision somebody wrote down.
    {
        let (_server, server) = serve(released, data, repo);
        let session = "worked-before-the-upgrade";
        hook(
            released,
            data,
            repo,
            &server,
            &event(session, "SessionStart", repo, json!({"source": "startup"})),
        );
        hook(
            released,
            data,
            repo,
            &server,
            &event(session, "UserPromptSubmit", repo, json!({"prompt": PROMPT})),
        );
        hook(
            released,
            data,
            repo,
            &server,
            &event(
                session,
                "PostToolUse",
                repo,
                json!({
                    "tool_name": "Bash",
                    "tool_input": {"command": "cargo test -p importer"},
                    "tool_response": {"stdout": "test result: ok. 9 passed", "exit_code": 0},
                }),
            ),
        );
        hook(
            released,
            data,
            repo,
            &server,
            &event(session, "SessionEnd", repo, json!({"reason": "exit"})),
        );
        let listed = wait_for_closed(released, data, repo);
        assert!(
            listed.contains(" closed "),
            "{version} did not close its session: {listed}"
        );

        let written = run(
            released,
            data,
            repo,
            [
                "write-page",
                "--path",
                DECISION,
                "--title",
                "The ledger stays in SQLite",
                "--body",
                "Keep the ledger in SQLite: the index is rebuilt from markdown, so nothing is lost with it.",
                "--tier",
                "semantic",
            ],
        );
        assert!(
            written.status.success(),
            "{version} could not write a page: {}",
            text(&written)
        );
    }

    // The upgrade: this binary's server on the same directory.
    let (server, address) = serve(current, data, repo);
    let handed = hook(
        current,
        data,
        repo,
        &address,
        &event(
            "first-after-the-upgrade",
            "SessionStart",
            repo,
            json!({"source": "startup"}),
        ),
    );
    assert!(
        handed.contains("empty amount") || handed.contains("importer"),
        "the note {version} left was not handed to the next session: {handed:?}"
    );

    let found = text(&run(current, data, repo, ["search", "ledger SQLite"]));
    assert!(
        found.contains(DECISION),
        "the decision written with {version} is not found: {found}"
    );

    let doctor = run(current, data, repo, ["doctor", "--server", &address]);
    assert!(
        doctor.status.success(),
        "doctor fails on memory {version} wrote: {}",
        text(&doctor)
    );
    drop(server);

    let check = run(current, data, repo, ["reindex", "--check"]);
    assert!(
        check.status.success(),
        "the index migrated from {version} is not what the wiki and the transcripts rebuild: {}",
        text(&check)
    );
}

#[test]
#[ignore = "needs released binaries, named in ANAMNESIS_UPGRADE_FROM"]
fn memory_a_release_wrote_survives_the_upgrade() {
    let named = std::env::var_os("ANAMNESIS_UPGRADE_FROM")
        .expect("ANAMNESIS_UPGRADE_FROM names the released binaries to upgrade from");
    let released: Vec<PathBuf> = std::env::split_paths(&named).collect();
    assert!(
        !released.is_empty(),
        "ANAMNESIS_UPGRADE_FROM names no binary"
    );
    for binary in &released {
        assert!(binary.is_file(), "{} is not a file", binary.display());
        upgrade_from(binary);
    }
}
