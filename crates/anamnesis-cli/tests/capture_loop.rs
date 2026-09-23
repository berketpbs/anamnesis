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
    serve_with_token(data, None)
}

fn serve_with_token(data: &Path, token: Option<&str>) -> (Server, String) {
    let mut command = anamnesis(data);
    if let Some(token) = token {
        command.env("ANAMNESIS_TOKEN", token);
    }
    let mut child = command
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
    hook_in(None, data, agent, server, payload)
}

/// [`hook`], started in `dir` — where a harness that runs a project's hooks
/// from its root starts them, and where the command looks for a project when
/// the payload names no directory it reads.
fn hook_in(dir: Option<&Path>, data: &Path, agent: &str, server: &str, payload: &Value) -> String {
    let mut command = anamnesis(data);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let mut child = command
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

/// Codex on Windows hands a hook's command to PowerShell. Run the command
/// `install-hooks` writes for it the way Codex runs it, and the session has to
/// reach the server. A quoted path followed by arguments is a syntax error in
/// PowerShell, and until this was known every Codex hook written on Windows
/// exited 1 before starting — `hook: SessionStart Failed` in Codex, nothing at
/// the server, and `/hooks` listing each one as approved.
#[cfg(windows)]
#[test]
fn the_codex_hook_command_runs_in_powershell() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = project("codex-in-powershell");
    let (_server, server) = serve(data.path());
    let installed = anamnesis(data.path())
        .current_dir(repo.path())
        .args([
            "install-hooks",
            "--agent",
            "codex",
            "--write",
            "--server",
            &server,
        ])
        .output()
        .expect("install-hooks runs");
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".codex/hooks.json")).expect("hooks.json"),
    )
    .expect("hooks.json parses");
    let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .expect("a command")
        .to_owned();

    let mut shell = Command::new("powershell.exe");
    for (name, _) in std::env::vars() {
        if name.starts_with("ANAMNESIS_") {
            shell.env_remove(name);
        }
    }
    let mut child = shell
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .env("ANAMNESIS_DATA_DIR", data.path())
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("powershell starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(
            event(
                "codex-in-powershell",
                "SessionStart",
                repo.path(),
                json!({"source": "startup"}),
            )
            .to_string()
            .as_bytes(),
        )
        .expect("write the payload");
    let output = child.wait_with_output().expect("powershell finishes");
    assert!(
        output.status.success(),
        "PowerShell could not run {command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    let listed = loop {
        let listed = anamnesis(data.path())
            .arg("sessions")
            .current_dir(repo.path())
            .output()
            .expect("sessions runs");
        let text = String::from_utf8_lossy(&listed.stdout).into_owned();
        if text.contains("codex") || Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(
        listed.contains("codex"),
        "the hook reached no session: {listed}"
    );
}

#[test]
fn codex_closing_reports_reach_the_wiki_and_the_next_claude_session() {
    let data = tempfile::tempdir().unwrap();
    let repo = project("codex-closing-reports");
    let (_server, server) = serve(data.path());
    let installed = anamnesis(data.path())
        .current_dir(repo.path())
        .args([
            "install-hooks",
            "--agent",
            "codex",
            "--write",
            "--server",
            &server,
        ])
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".codex/hooks.json")).unwrap(),
    )
    .unwrap();
    for name in ["Stop", "SubagentStop"] {
        assert!(
            settings["hooks"][name].is_array(),
            "{name} is not installed"
        );
    }
    hook(
        data.path(),
        "codex",
        &server,
        &event("writer", "SessionStart", repo.path(), json!({})),
    );
    let conclusion = "Keep SQLite: the index can be rebuilt from markdown.";
    let report = "The parser investigation found an empty currency field.";
    for (name, extra) in [
        (
            "Stop",
            json!({"last_assistant_message": conclusion, "stop_hook_active": false}),
        ),
        (
            "SubagentStop",
            json!({"last_assistant_message": report, "agent_id": "investigator", "agent_type": "explorer"}),
        ),
    ] {
        let reply = hook(
            data.path(),
            "codex",
            &server,
            &event("writer", name, repo.path(), extra),
        );
        assert_eq!(
            serde_json::from_str::<Value>(&reply).unwrap(),
            json!({}),
            "closing capture must not request another turn"
        );
    }
    hook(
        data.path(),
        "codex",
        &server,
        &event("writer", "SessionEnd", repo.path(), json!({})),
    );
    assert!(wait_for_closed(data.path(), repo.path(), 1).contains("codex"));
    let pages = data
        .path()
        .join("wiki/default/codex-closing-reports/sessions");
    let page = std::fs::read_dir(pages)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let body = std::fs::read_to_string(page).unwrap();
    assert!(body.contains(conclusion), "{body}");
    assert!(body.contains(report), "{body}");
    let handed = hook(
        data.path(),
        "claude-code",
        &server,
        &event("reader", "SessionStart", repo.path(), json!({})),
    );
    assert!(
        handed.contains(conclusion),
        "Codex's decision must reach Claude: {handed}"
    );
}

#[test]
fn a_managed_launch_probes_capture_and_its_hook_uses_the_selected_server() {
    let data = tempfile::tempdir().unwrap();
    let repo = project("managed-capture");
    let token = "capture-test-token";
    let (_server, server) = serve_with_token(data.path(), Some(token));
    let installed = anamnesis(data.path())
        .current_dir(repo.path())
        .args([
            "install-hooks",
            "--agent",
            "codex",
            "--write",
            "--server",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();
    assert!(installed.status.success());

    // /health succeeds without this token. The old launcher ran the program
    // anyway; use an absent program so an attempted spawn is observable.
    let refused = anamnesis(data.path())
        .current_dir(repo.path())
        .args([
            "run",
            "codex",
            "--server",
            &server,
            "--program",
            "anamnesis-no-such-test-program",
        ])
        .output()
        .unwrap();
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(refusal.contains("capture preflight failed"), "{refusal}");
    assert!(
        !refusal.contains("could not find"),
        "the child must not start: {refusal}"
    );

    // Act as a harness that invokes its installed hook. The old baked-in
    // endpoint is unreachable; only the managed override can deliver it.
    let mut child = anamnesis(data.path())
        .current_dir(repo.path())
        .args([
            "run",
            "codex",
            "--server",
            &server,
            "--token",
            token,
            "--program",
            env!("CARGO_BIN_EXE_anamnesis"),
            "--",
            "--data-dir",
        ])
        .arg(data.path())
        .args(["hook", "--agent", "codex", "--server", "http://127.0.0.1:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            event(
                "managed-writer",
                "UserPromptSubmit",
                repo.path(),
                json!({"prompt": "Retain the managed endpoint decision."}),
            )
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("user-prompt"),
        "the probe must classify a real prompt"
    );
    let store = anamnesis_store::Store::open(data.path().join("db/anamnesis.db")).unwrap();
    let scope = anamnesis_core::scope::resolve_scope(repo.path()).unwrap();
    let sessions = store.recent_sessions(scope.project_id, 10).unwrap();
    assert_eq!(sessions.len(), 1, "the probe must not create a session");
    let observations = store.observations(sessions[0].id).unwrap();
    assert_eq!(
        observations.len(),
        1,
        "the hook must reach the selected server once"
    );
    assert!(
        observations[0]
            .body
            .as_str()
            .contains("managed endpoint decision")
    );
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

/// A terminal closed rather than ended sends no `SessionEnd`, and the agent
/// opened beside it a moment later is the one person who needs the note. The
/// threshold is set to a second so the test does not wait out two minutes.
#[test]
fn a_session_whose_terminal_was_closed_is_handed_to_the_next_one() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        repo.path().join(".anamnesis.toml"),
        "[scope]
workspace = \"default\"
project = \"closed-terminal\"

[sessions]
handover_after_seconds = 1
",
    )
    .expect("marker");
    let (_server, server) = serve(data.path());

    // Everything but the end: the terminal was closed.
    for payload in [
        event(
            "the-closed-terminal",
            "SessionStart",
            repo.path(),
            json!({"source": "startup"}),
        ),
        event(
            "the-closed-terminal",
            "UserPromptSubmit",
            repo.path(),
            json!({"prompt": "Make the importer skip rows with an empty amount."}),
        ),
        event(
            "the-closed-terminal",
            "PostToolUse",
            repo.path(),
            json!({
                "tool_name": "Bash",
                "tool_input": {"command": "cargo test -p importer"},
                "tool_response": {"stdout": "test result: ok. 9 passed", "exit_code": 0},
            }),
        ),
    ] {
        hook(data.path(), "claude-code", &server, &payload);
    }
    let listed = anamnesis(data.path())
        .arg("sessions")
        .current_dir(repo.path())
        .output()
        .expect("sessions runs");
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(!listed.contains(" closed "), "{listed}");

    std::thread::sleep(Duration::from_millis(2500));

    let handed = hook(
        data.path(),
        "codex",
        &server,
        &event(
            "the-next-terminal",
            "SessionStart",
            repo.path(),
            json!({"source": "startup"}),
        ),
    );
    assert!(handed.contains("empty amount"), "{handed:?}");
    assert!(handed.contains("still open"), "{handed:?}");

    let again = hook(
        data.path(),
        "claude-code",
        &server,
        &event(
            "a-third-terminal",
            "SessionStart",
            repo.path(),
            json!({"source": "startup"}),
        ),
    );
    assert!(
        !again.contains("empty amount"),
        "a note is handed once: {again:?}"
    );
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

// ---------------------------------------------------------------------------
// One project, four harnesses. Every other test here stays inside one harness,
// and every bug the cross-harness run of 2026-09-18 found was in a hand-over
// between two of them: a Cursor session that claimed its handoff under a name
// no session had (#274), recall printed where Cursor cannot read it and
// answered to Gemini under the wrong event (#275), and a notification the
// harness submitted asked about as though somebody had typed it (#272). The
// first of those printed the right handoff to the right harness; only the
// index said who it had been given to.
//
// The payloads are each harness's documented shape, not recordings: Codex's
// fields are Claude Code's with a `model`, Gemini CLI names its own events,
// and Cursor sends `conversation_id` and `workspace_roots` where the others
// send `session_id` and `cwd`, with a `session_id` of its own on the start
// and the end that matches nothing else.

/// Run a query against the index, once the server that holds it has stopped.
fn index_rows<T>(data: &Path, sql: &str, row: fn(&rusqlite::Row<'_>) -> T) -> Vec<T> {
    let store = anamnesis_store::Store::open(data.join("db").join("anamnesis.db"))
        .expect("the index opens");
    let conn = store.connection();
    let mut statement = conn.prepare(sql).expect("the query prepares");
    statement
        .query_map([], |r| Ok(row(r)))
        .expect("the query runs")
        .map(|r| r.expect("a row"))
        .collect()
}

/// The page under `wiki/<workspace>/<project>/` whose text contains `needle`,
/// as recall names it.
fn page_saying(data: &Path, project: &str, needle: &str) -> String {
    let sessions = data.join("wiki/default").join(project).join("sessions");
    std::fs::read_dir(&sessions)
        .expect("session pages")
        .map(|entry| entry.expect("an entry").path())
        .find(|path| std::fs::read_to_string(path).is_ok_and(|text| text.contains(needle)))
        .map(|path| {
            format!(
                "sessions/{}",
                path.file_name().expect("a name").to_string_lossy()
            )
        })
        .unwrap_or_else(|| panic!("no page in {project} says {needle:?}"))
}

fn cursor(conversation: &str, name: &str, repo: &Path, extra: Value) -> Value {
    let mut payload = json!({
        "conversation_id": conversation,
        "generation_id": format!("{conversation}-generation"),
        "hook_event_name": name,
        "workspace_roots": [repo.to_string_lossy()],
    });
    if let (Some(base), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    payload
}

/// What the server would recall for `asked`, asked directly rather than
/// through a hook — to tell a hook that stayed quiet from a server with
/// nothing to say.
fn recall(server: &str, agent: &str, session: &str, cwd: &Path, asked: &str) -> String {
    let cwd = cwd.to_string_lossy();
    reqwest::blocking::Client::new()
        .get(format!("{server}/recall"))
        .query(&[
            ("agent", agent),
            ("session_id", session),
            ("cwd", cwd.as_ref()),
            ("q", asked),
        ])
        .send()
        .and_then(|response| response.text())
        .unwrap_or_default()
}

/// What a Gemini CLI reply carries, and the event it says it answers.
fn gemini_context(printed: &str) -> Option<(String, String)> {
    let reply: Value = serde_json::from_str(printed.trim()).ok()?;
    let output = &reply["hookSpecificOutput"];
    Some((
        output["hookEventName"].as_str()?.to_owned(),
        output["additionalContext"].as_str()?.to_owned(),
    ))
}

/// A decision made in Claude Code reaches each harness in turn, in the shape
/// that harness reads, and comes back to Claude Code four sessions later.
///
/// Two routes carry it, and the test holds both apart. A handoff carries one
/// session to the next and no further. What the first session decided comes
/// back after that only through recall, and only by a name its page carries.
///
/// Each harness asks by a different one of those names, because asking is
/// recorded: once a second page carries a name, a project this small counts it
/// as common vocabulary and it names nothing. A name on one page always does.
///
/// Every broken hop is collected before anything fails, so a regression that
/// breaks two harnesses says so in one run.
#[test]
fn a_decision_travels_from_claude_through_codex_gemini_and_cursor_and_back() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = project("handover-across-agents");
    let (server_process, server) = serve(data.path());
    let d = data.path();
    let r = repo.path();
    let importer = r.join("ledger").join("importer.py");
    let edit = json!({
        "tool_name": "Edit",
        "tool_input": {"file_path": importer.to_string_lossy()},
        "tool_response": {"success": true},
    });
    let decision = "Every log line goes through redact_amounts before LEDGER_LOG_SINK, and a \
                    failed row is reported by mask_failed_row: the logs are shipped to a third \
                    party.";
    let gemini_asks = "Does redact_amounts cover the importer's failure path too?";
    let cursor_asks = "Where does LEDGER_LOG_SINK send the importer's lines?";
    let claude_asks = "What does mask_failed_row keep out of the log?";
    let mut broken: Vec<String> = Vec::new();

    // Claude Code decides.
    let s1 = "11111111-1111-4111-8111-111111111111";
    let claude = |session: &str, name: &str, extra: Value| {
        hook(d, "claude-code", &server, &event(session, name, r, extra))
    };
    claude(s1, "SessionStart", json!({"source": "startup"}));
    claude(
        s1,
        "UserPromptSubmit",
        json!({"prompt": "Add logging to the importer. Amounts must never appear in a log line."}),
    );
    claude(s1, "PostToolUse", edit.clone());
    claude(s1, "Stop", json!({"last_assistant_message": decision}));
    claude(s1, "SessionEnd", json!({"reason": "exit"}));
    wait_for_closed(d, r, 1);
    let planted = page_saying(d, "handover-across-agents", "shipped to a third party");

    // Codex is handed it as plain text, which is what Codex injects.
    let s2 = "22222222-2222-4222-8222-222222222222";
    let codex = |name: &str, extra: Value| {
        let mut payload = event(s2, name, r, extra);
        payload["model"] = json!("gpt-5");
        hook(d, "codex", &server, &payload)
    };
    let handed = codex("SessionStart", json!({"source": "startup"}));
    if !handed.contains("shipped to a third party") || handed.trim_start().starts_with('{') {
        broken.push(format!(
            "Codex was not handed Claude's decision as text: {handed:?}"
        ));
    }
    codex(
        "UserPromptSubmit",
        json!({"prompt": "Log a warning whenever the importer skips a malformed row."}),
    );
    codex("PostToolUse", edit.clone());
    codex(
        "Stop",
        json!({
            "last_assistant_message": "Skipped rows now log a warning naming the line number, never the amount.",
            "stop_hook_active": false,
        }),
    );
    codex("SessionEnd", json!({}));
    wait_for_closed(d, r, 2);

    // Gemini CLI is handed Codex's session inside one JSON object, and its
    // prompt is answered under the event it asked on.
    let s3 = "33333333-3333-4333-8333-333333333333";
    let gemini =
        |name: &str, extra: Value| hook(d, "gemini-cli", &server, &event(s3, name, r, extra));
    let handed = gemini("SessionStart", json!({"source": "startup"}));
    match gemini_context(&handed) {
        Some((event, context))
            if event == "SessionStart" && context.contains("never the amount") => {}
        _ => broken.push(format!("Gemini was not handed Codex's session: {handed:?}")),
    }
    let recalled = gemini("BeforeAgent", json!({"prompt": gemini_asks}));
    match gemini_context(&recalled) {
        Some((event, context)) if event == "BeforeAgent" && context.contains(&planted) => {}
        _ => broken.push(format!(
            "Gemini's prompt was not answered with {planted} under BeforeAgent: {recalled:?}"
        )),
    }
    gemini("AfterTool", edit.clone());
    gemini("SessionEnd", json!({"reason": "exit"}));
    wait_for_closed(d, r, 3);

    // Cursor is handed Gemini's session under its own conversation, and told
    // nothing at the prompt, which has no field to hear it in — although the
    // server has an answer for that prompt, or the silence would prove nothing.
    let c4 = "44444444-4444-4444-8444-444444444444";
    let own_start = json!({"session_id": "cursor-start-and-end-only"});
    // Cursor runs a project's hooks from the project root, as its
    // documentation says; started anywhere else, a regression that reads the
    // wrong fields loses the note outright instead of doing what it did in
    // use — delivering it, and recording it as claimed by nobody.
    let cursor_hook = |payload: Value| hook_in(Some(r), d, "cursor", &server, &payload);
    let handed = cursor_hook(cursor(c4, "sessionStart", r, own_start.clone()));
    let context =
        serde_json::from_str::<Value>(handed.trim()).unwrap_or_default()["additional_context"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
    if !context.contains(gemini_asks) {
        broken.push(format!(
            "Cursor was not handed Gemini's session: {handed:?}"
        ));
    }
    let answer = recall(&server, "cursor", c4, r, cursor_asks);
    if !answer.contains(&planted) {
        broken.push(format!(
            "the server had no answer for Cursor's prompt, so its silence proves nothing: {answer:?}"
        ));
    }
    let printed = cursor_hook(cursor(
        c4,
        "beforeSubmitPrompt",
        r,
        json!({"prompt": cursor_asks, "attachments": []}),
    ));
    if !printed.trim().is_empty() {
        broken.push(format!(
            "Cursor's prompt hook printed what it cannot read: {printed:?}"
        ));
    }
    cursor_hook(cursor(
        c4,
        "postToolUse",
        r,
        json!({
            "cwd": r.to_string_lossy(),
            "tool_name": "edit_file",
            "tool_input": {"file_path": importer.to_string_lossy()},
            "tool_output": "{\"success\": true}",
        }),
    ));
    cursor_hook(cursor(c4, "sessionEnd", r, own_start));
    wait_for_closed(d, r, 4);

    // Claude Code is handed Cursor's session, and the decision four sessions
    // back comes to it only when asked by name — never for a notification.
    let s5 = "55555555-5555-4555-8555-555555555555";
    let handed = claude(s5, "SessionStart", json!({"source": "startup"}));
    if !handed.contains("(cursor,") || !handed.contains(cursor_asks) {
        broken.push(format!(
            "Claude was not handed Cursor's session: {handed:?}"
        ));
    }
    if handed.contains("shipped to a third party") {
        broken.push(format!(
            "a handoff carried more than one session: {handed:?}"
        ));
    }
    // A notification the harness submitted is not a question, whatever it
    // says. This one says what would be answered if somebody had typed it,
    // which is the only case where not asking matters: wrapped in tags the
    // project has never written, most notifications fall short of the naming
    // gate without any filter. The server refuses one too, for hooks older
    // than the one that stopped sending it.
    let notification = format!("<task-notification>\n{claude_asks}\n</task-notification>");
    let answer = recall(&server, "claude-code", s5, r, &notification);
    if !answer.is_empty() {
        broken.push(format!(
            "the server answered a harness notification: {answer:?}"
        ));
    }
    let notified = claude(s5, "UserPromptSubmit", json!({"prompt": notification}));
    if !notified.is_empty() {
        broken.push(format!("a harness notification was answered: {notified:?}"));
    }
    let recalled = claude(s5, "UserPromptSubmit", json!({"prompt": claude_asks}));
    if !recalled.contains(&planted) {
        broken.push(format!(
            "Claude's question did not recall {planted}: {recalled:?}"
        ));
    }

    // Each handoff went to the session that asked for it: a session of that
    // agent, holding that harness's events — not one derived from a name the
    // harness never sent, which prints the same note and records nothing.
    drop(server_process);
    let claims = index_rows(
        d,
        "SELECT s.agent, (SELECT COUNT(*) FROM observations o WHERE o.session_id = s.id)
         FROM handoffs h LEFT JOIN sessions s ON s.id = h.to_session
         WHERE h.state = 'accepted' ORDER BY h.accepted_at",
        |row| {
            (
                row.get::<_, Option<String>>(0).ok().flatten(),
                row.get::<_, i64>(1).unwrap_or(0),
            )
        },
    );
    let agents: Vec<Option<&str>> = claims.iter().map(|(agent, _)| agent.as_deref()).collect();
    if agents
        != [
            Some("codex"),
            Some("gemini-cli"),
            Some("cursor"),
            Some("claude-code"),
        ]
        || claims.iter().any(|(_, observations)| *observations == 0)
    {
        broken.push(format!("handoffs were claimed by {claims:?}"));
    }
    let subjects = index_rows(
        d,
        "SELECT subject FROM audit_log WHERE action = 'handoff.claimed' ORDER BY at",
        |row| row.get::<_, String>(0).unwrap_or_default(),
    );
    if subjects != [s2, s3, c4, s5] {
        broken.push(format!("the audit log names the claimants {subjects:?}"));
    }
    let sessions = index_rows(
        d,
        "SELECT agent, COUNT(*) FROM sessions GROUP BY agent ORDER BY agent",
        |row| {
            (
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, i64>(1).unwrap_or(0),
            )
        },
    );
    let expected = [
        ("claude-code", 2),
        ("codex", 1),
        ("cursor", 1),
        ("gemini-cli", 1),
    ];
    if sessions.iter().map(|(a, n)| (a.as_str(), *n)).ne(expected) {
        broken.push(format!("sessions recorded per agent: {sessions:?}"));
    }

    assert!(broken.is_empty(), "\n{}", broken.join("\n"));
}

/// Two projects on one server: each is handed its own last session and
/// recalled from its own pages, with the other's page one query away.
///
/// The positive half is what makes the negative one worth anything — the
/// question that finds nothing at home finds the neighbour's page in the
/// neighbour.
#[test]
fn a_neighbouring_project_is_never_handed_or_recalled() {
    let data = tempfile::tempdir().expect("data dir");
    let home = project("handover-home");
    let neighbour = project("handover-neighbour");
    let (_server, server) = serve(data.path());
    let d = data.path();
    let asked = "Which host does deploy_staging_ledger push to?";

    let finish = |repo: &Path, session: &str, said: &str| {
        let claude = |name: &str, extra: Value| {
            hook(
                d,
                "claude-code",
                &server,
                &event(session, name, repo, extra),
            )
        };
        claude("SessionStart", json!({"source": "startup"}));
        claude(
            "UserPromptSubmit",
            json!({"prompt": "Write down how staging is deployed."}),
        );
        claude(
            "PostToolUse",
            json!({
                "tool_name": "Edit",
                "tool_input": {"file_path": repo.join("DEPLOY.md").to_string_lossy()},
                "tool_response": {"success": true},
            }),
        );
        claude("Stop", json!({"last_assistant_message": said}));
        claude("SessionEnd", json!({"reason": "exit"}));
        wait_for_closed(d, repo, 1);
    };
    // The neighbour finishes last, so its handoff is the newest one waiting
    // anywhere: a claim that forgot which project asked would take it.
    finish(
        home.path(),
        "home-one",
        "Staging is deployed by hand from the release branch.",
    );
    finish(
        neighbour.path(),
        "neighbour-one",
        "deploy_staging_ledger pushes to ledger-stg-02 with ENV=staging.",
    );
    let theirs = page_saying(d, "handover-neighbour", "ledger-stg-02");

    let start = |repo: &Path, session: &str| {
        hook(
            d,
            "claude-code",
            &server,
            &event(session, "SessionStart", repo, json!({"source": "startup"})),
        )
    };
    let ask = |repo: &Path, session: &str| {
        hook(
            d,
            "claude-code",
            &server,
            &event(session, "UserPromptSubmit", repo, json!({"prompt": asked})),
        )
    };

    let handed = start(home.path(), "home-two");
    assert!(
        handed.contains("by hand"),
        "home is handed its own session: {handed:?}"
    );
    assert!(
        !handed.contains("ledger-stg-02"),
        "home was handed the neighbour's: {handed:?}"
    );
    let recalled = ask(home.path(), "home-two");
    assert!(
        !recalled.contains(&theirs),
        "home recalled the neighbour's page: {recalled:?}"
    );

    let handed = start(neighbour.path(), "neighbour-two");
    assert!(
        handed.contains("ledger-stg-02"),
        "the neighbour keeps its own handoff: {handed:?}"
    );
    let recalled = ask(neighbour.path(), "neighbour-two");
    assert!(
        recalled.contains(&theirs),
        "the same question finds the page where it lives: {recalled:?}"
    );
}
