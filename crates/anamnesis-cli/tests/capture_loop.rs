//! The loop the whole project exists for, run with the built binary: a server,
//! a session reported through `anamnesis hook` the way a harness runs it, and
//! the next session handed what the last one left.
//!
//! Every piece has tests of its own, and none of them start the process a
//! harness starts. The hook reads a payload on stdin, delivers it within a
//! second, and writes the handoff to stdout in the shape each harness reads:
//! plain text for Claude Code, one JSON object on every event for Gemini CLI.
//! This runs that on every platform CI builds, Windows included.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A command with this machine's anamnesis settings taken out of its
/// environment, so the result does not depend on who ran the test.
fn anamnesis(data: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anamnesis"));
    for (name, _) in std::env::vars() {
        if name.starts_with("ANAMNESIS_")
            || name.ends_with("_API_KEY")
            || name == "ANTHROPIC_API_KEY"
        {
            command.env_remove(name);
        }
    }
    // Keys are looked up in the credential store under this service name, and
    // there are none under it: the server counts rather than calling a model.
    command.env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-capture-loop");
    command.arg("--data-dir").arg(data);
    command
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start a server on a free port and return its address, read from the banner.
fn serve(data: &Path) -> (Server, String) {
    let mut child = anamnesis(data)
        .args(["serve", "--port", "0", "--no-watch", "--no-ui"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server starts");
    let stdout = child.stdout.take().expect("stdout");
    let server = Server(child);
    let first = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .find(|line| line.contains("serving on "))
        .expect("the banner names the address");
    let address = first
        .split("serving on ")
        .nth(1)
        .expect("an address")
        .trim()
        .to_owned();
    (server, address)
}

/// Run one hook the way a harness does, and return what it printed.
fn hook(data: &Path, agent: &str, server: &str, payload: &Value) -> String {
    let mut child = anamnesis(data)
        .args(["hook", "--agent", agent, "--server", server])
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
    assert!(
        output.status.success(),
        "a hook always exits 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is text")
}

fn event(session: &str, name: &str, cwd: &Path, extra: Value) -> Value {
    let mut payload = json!({
        "session_id": session,
        "hook_event_name": name,
        "cwd": cwd.to_string_lossy(),
    });
    if let (Some(base), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    payload
}

/// A worked session: a prompt, a tool call, and the end.
fn work(data: &Path, agent: &str, server: &str, session: &str, repo: &Path) -> Vec<String> {
    vec![
        hook(
            data,
            agent,
            server,
            &event(session, "SessionStart", repo, json!({"source": "startup"})),
        ),
        hook(
            data,
            agent,
            server,
            &event(
                session,
                "UserPromptSubmit",
                repo,
                json!({"prompt": "Make the importer skip rows with an empty amount."}),
            ),
        ),
        hook(
            data,
            agent,
            server,
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
        ),
        hook(
            data,
            agent,
            server,
            &event(session, "SessionEnd", repo, json!({"reason": "exit"})),
        ),
    ]
}

/// What `sessions` lists for this project, once it shows `closed` sessions.
fn wait_for_closed(data: &Path, repo: &Path, count: usize) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let listed = anamnesis(data)
            .arg("sessions")
            .current_dir(repo)
            .output()
            .expect("sessions runs");
        let text = String::from_utf8_lossy(&listed.stdout).into_owned();
        if text.matches(" closed ").count() >= count || Instant::now() > deadline {
            return text;
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

#[test]
fn claude_code_records_a_session_and_the_next_one_is_handed_its_note() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = project("capture-loop");
    let (_server, server) = serve(data.path());

    let printed = work(
        data.path(),
        "claude-code",
        &server,
        "claude-session-one",
        repo.path(),
    );
    assert!(
        printed.iter().all(String::is_empty),
        "nothing is printed while a session with no note waiting works: {printed:?}"
    );

    let listed = wait_for_closed(data.path(), repo.path(), 1);
    assert!(listed.contains("claude-code"), "{listed}");
    assert!(
        listed.contains("4 obs"),
        "the prompt, the tool call and both ends: {listed}"
    );

    let handed = hook(
        data.path(),
        "claude-code",
        &server,
        &event(
            "claude-session-two",
            "SessionStart",
            repo.path(),
            json!({"source": "startup"}),
        ),
    );
    assert!(
        handed.contains("empty amount") || handed.contains("importer"),
        "the next session starts with what the last one did: {handed:?}"
    );

    let again = hook(
        data.path(),
        "claude-code",
        &server,
        &event(
            "claude-session-three",
            "SessionStart",
            repo.path(),
            json!({"source": "startup"}),
        ),
    );
    assert!(again.is_empty(), "a note is handed once: {again:?}");
}

/// Gemini CLI parses stdout as one JSON object on every event, so every event
/// gets one — empty when there is nothing to say — and the note rides in it.
#[test]
fn gemini_cli_is_answered_with_one_json_object_on_every_event() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = project("capture-loop-gemini");
    let (_server, server) = serve(data.path());

    for printed in work(
        data.path(),
        "gemini-cli",
        &server,
        "gemini-session-one",
        repo.path(),
    ) {
        let reply: Value = serde_json::from_str(printed.trim())
            .unwrap_or_else(|_| panic!("not one JSON object: {printed:?}"));
        assert!(reply.is_object(), "{printed:?}");
    }
    wait_for_closed(data.path(), repo.path(), 1);

    let handed = hook(
        data.path(),
        "gemini-cli",
        &server,
        &event(
            "gemini-session-two",
            "SessionStart",
            repo.path(),
            json!({"source": "startup"}),
        ),
    );
    let reply: Value = serde_json::from_str(handed.trim()).expect("one JSON object");
    let context = reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(
        context.contains("empty amount") || context.contains("importer"),
        "the note is in the field Gemini CLI reads: {handed:?}"
    );
}

/// A port nothing is listening on, a moment ago.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port()
}

/// Whether a server answers at `server`, asked the way a health check asks.
fn answers(data: &Path, server: &str) -> bool {
    anamnesis(data)
        .args(["hook", "--probe", "--server", server])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A wrapper that starts the server, reads what it needs from the banner and
/// stops reading. Here stdout is closed before the first line, so the banner
/// meets a closed pipe every time: the server used to panic on it and exit 101.
#[test]
fn a_server_whose_stdout_nobody_reads_keeps_serving() {
    let data = tempfile::tempdir().expect("data dir");
    let port = free_port();
    let server = format!("http://127.0.0.1:{port}");
    let mut child = anamnesis(data.path())
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--no-watch",
            "--no-ui",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server starts");
    drop(child.stdout.take());
    let guard = Server(child);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut up = false;
    while Instant::now() < deadline {
        if answers(data.path(), &server) {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut guard = guard;
    assert_eq!(
        guard.0.try_wait().expect("the process can be asked"),
        None,
        "the server exited because nobody read its banner"
    );
    assert!(up, "the server does not answer at {server}");
}

/// A hook promises exit 0 whatever happens, and a harness that has stopped
/// reading is part of whatever: here stdout is closed before the handoff is
/// printed to it.
#[test]
fn a_hook_whose_stdout_is_closed_still_exits_zero() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = project("capture-loop-closed-stdout");
    let (_server, server) = serve(data.path());
    work(
        data.path(),
        "claude-code",
        &server,
        "closed-one",
        repo.path(),
    );
    wait_for_closed(data.path(), repo.path(), 1);

    let mut child = anamnesis(data.path())
        .args(["hook", "--agent", "claude-code", "--server", &server])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the hook starts");
    drop(child.stdout.take());
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(
            event(
                "closed-two",
                "SessionStart",
                repo.path(),
                json!({"source": "startup"}),
            )
            .to_string()
            .as_bytes(),
        )
        .expect("write the payload");
    let status = child.wait().expect("the hook finishes");
    assert!(status.success(), "a hook exits 0 even unread: {status:?}");
}
