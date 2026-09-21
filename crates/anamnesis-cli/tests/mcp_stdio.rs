//! The MCP server as a harness meets it: the built binary, over stdio.
//!
//! The crate's own tests call the tool handlers as functions, which says
//! nothing about the three things a harness depends on and a function call
//! cannot reach: that the process speaks the protocol on stdin and stdout, that
//! nothing but protocol frames ever reaches stdout — a single log line there
//! breaks the stream for good — and that the tools a harness lists are the ones
//! that answer.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

/// Long enough for a debug build to open a store and migrate it.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Receiver<String>,
}

impl Server {
    fn start(data: &Path, repo: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anamnesis"));
        command
            .arg("--data-dir")
            .arg(data)
            .arg("mcp")
            .arg("--repo")
            .arg(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // What this machine's shell exports is not what the test is about: a
        // hosted embedder or a data directory from the environment would make
        // the result depend on who ran it.
        for (name, _) in std::env::vars() {
            if name.starts_with("ANAMNESIS_") {
                command.env_remove(name);
            }
        }
        command.env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-mcp-stdio");
        let mut child = command.spawn().expect("the binary starts");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let (err_sender, err_lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if err_sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            stderr: err_lines,
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write a frame");
        self.stdin.flush().expect("flush");
    }

    /// The response to request `id`. Every line read on the way must be a
    /// JSON-RPC frame: anything else on stdout is the failure this file is for.
    fn answer(&mut self, id: u64) -> Value {
        loop {
            let line = match self.lines.recv_timeout(ANSWER_WITHIN) {
                Ok(line) => line,
                Err(_) => {
                    let said: Vec<String> = self.stderr.try_iter().collect();
                    panic!("no answer to request {id}; stderr:\n{}", said.join("\n"));
                }
            };
            let frame: Value = serde_json::from_str(&line).unwrap_or_else(|_| {
                panic!("stdout carried something that is not a frame: {line:?}")
            });
            assert_eq!(frame["jsonrpc"], "2.0", "{line}");
            if frame["id"] == json!(id) {
                return frame;
            }
        }
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }));
        let frame = self.answer(id);
        assert!(frame.get("error").is_none(), "{tool} failed: {frame}");
        assert_ne!(frame["result"]["isError"], true, "{tool} failed: {frame}");
        frame["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of a tool result, all its content blocks joined.
fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[test]
fn a_harness_can_list_write_find_and_read_over_stdio() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        repo.path().join(".anamnesis.toml"),
        "[scope]\nworkspace = \"default\"\nproject = \"stdio-check\"\n",
    )
    .expect("marker");

    let mut server = Server::start(data.path(), repo.path());

    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "anamnesis-stdio-test", "version": "0" },
        },
    }));
    let initialized = server.answer(1);
    assert!(
        initialized["result"]["capabilities"]["tools"].is_object(),
        "the server offers tools: {initialized}"
    );
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    server.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let listed = server.answer(2);
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for tool in [
        "memory_query",
        "memory_write_page",
        "memory_read_page",
        "memory_handoff_accept",
        "workstream_start",
        "workstream_status",
    ] {
        assert!(names.contains(&tool), "{tool} is not listed: {names:?}");
    }

    let written = server.call(
        3,
        "memory_write_page",
        json!({
            "path": "gotchas/stdio-is-for-frames.md",
            "title": "Stdout is for protocol frames",
            "body": "A log line printed to stdout breaks the MCP stream for the rest of the session.",
            "entities": ["stdout", "mcp"],
        }),
    );
    assert!(
        text_of(&written).contains("gotchas/stdio-is-for-frames.md"),
        "{written}"
    );

    let found = server.call(
        4,
        "memory_query",
        json!({ "text": "log line stdout stream" }),
    );
    assert!(
        text_of(&found).contains("gotchas/stdio-is-for-frames.md"),
        "the page just written is found: {found}"
    );

    let read = server.call(
        5,
        "memory_read_page",
        json!({ "path": "gotchas/stdio-is-for-frames.md" }),
    );
    assert!(
        text_of(&read).contains("breaks the MCP stream"),
        "the whole body comes back: {read}"
    );

    // An unknown tool is refused as a protocol answer, not by a crash that
    // takes the stream down with it.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": { "name": "memory_nonexistent", "arguments": {} },
    }));
    let refused = server.answer(6);
    assert!(
        refused.get("error").is_some() || refused["result"]["isError"] == true,
        "{refused}"
    );
    server.send(&json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }));
    assert!(
        server.answer(7)["result"]["tools"].is_array(),
        "and it still answers"
    );
}

/// Each tool tells the harness whether it changes memory, in the listing the
/// harness reads before deciding whether to ask. A harness with nobody to ask
/// refuses a tool that says nothing: `codex exec` refused `memory_query` on
/// 2026-09-22 and its agent grepped the wiki's files instead.
#[test]
fn every_tool_says_whether_it_changes_memory() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        repo.path().join(".anamnesis.toml"),
        "[scope]\nworkspace = \"default\"\nproject = \"stdio-hints\"\n",
    )
    .expect("marker");

    let mut server = Server::start(data.path(), repo.path());
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "anamnesis-stdio-test", "version": "0" },
        },
    }));
    server.answer(1);
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    server.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let listed = server.answer(2);
    let tools = listed["result"]["tools"].as_array().expect("a tool list");

    let reads = ["memory_query", "memory_read_page", "workstream_status"];
    let writes = [
        "memory_write_page",
        "memory_handoff_accept",
        "workstream_start",
    ];
    for tool in tools {
        let name = tool["name"].as_str().expect("a name");
        let hints = &tool["annotations"];
        let expected = if reads.contains(&name) {
            true
        } else if writes.contains(&name) {
            false
        } else {
            panic!("{name} is listed and neither a read nor a write here: add it to one");
        };
        assert_eq!(
            hints["readOnlyHint"],
            json!(expected),
            "{name} should say readOnlyHint = {expected}: {tool}"
        );
        if !expected {
            // A write that replaces or claims, never one that loses anything:
            // pages keep their history in git, a handoff is consumed once.
            assert_eq!(hints["destructiveHint"], json!(false), "{name}: {tool}");
        }
        assert_eq!(
            hints["openWorldHint"],
            json!(false),
            "{name} reaches nothing outside this memory: {tool}"
        );
    }
    assert_eq!(tools.len(), reads.len() + writes.len(), "{listed}");
}
