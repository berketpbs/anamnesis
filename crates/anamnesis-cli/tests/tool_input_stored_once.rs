//! A tool call's input is kept once in the index and twice in the transcript,
//! and a rebuild arrives at the same index.
//!
//! The server stores an attempt's body empty once its completion repeats it;
//! the transcript keeps both halves as they arrived, because it is
//! append-only and the source the index is rebuilt from. `reindex --check`
//! compares every observation byte for byte, so a rebuild that did not apply
//! the same rule would report each settled attempt as drift — and a rebuild
//! from nothing but the transcript would bring every duplicate back.
//!
//! Driven through the binary, because the two halves of the rule live in
//! different crates: the capture path settles, and `reindex` replays.

use std::path::Path;
use std::process::Command;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::{BoundedBody, EventKind, ToolRef};
use anamnesis_core::scope::resolve_scope;
use anamnesis_core::session::AgentKind;
use anamnesis_store::{RawSpool, Store, new_observation, new_session};

/// A command with this machine's anamnesis settings taken out of its
/// environment, so the result does not depend on who ran the test.
fn anamnesis(cwd: &Path, data: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anamnesis"));
    for (name, _) in std::env::vars() {
        if name.starts_with("ANAMNESIS_")
            || name.ends_with("_API_KEY")
            || name == "ANTHROPIC_API_KEY"
        {
            command.env_remove(name);
        }
    }
    command.env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-tool-input");
    command.current_dir(cwd);
    command.arg("--data-dir").arg(data);
    command
}

fn run(command: &mut Command) -> (bool, String) {
    let out = command.output().expect("the command runs");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

fn tool(call_id: &str) -> Option<ToolRef> {
    Some(ToolRef {
        name: "Bash".to_owned(),
        ok: None,
        call_id: Some(call_id.to_owned()),
    })
}

#[test]
fn a_settled_attempt_rebuilds_to_what_the_live_index_holds() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    std::fs::write(
        repo.join(".anamnesis.toml"),
        "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
    )
    .expect("marker");
    let data_path = dir.path().join("data");
    let data = DataDir::resolve(Some(data_path.clone())).expect("data dir");
    data.ensure_layout().expect("layout");

    let scope = resolve_scope(&repo).expect("scope");
    let session_id = SessionId::derive(scope.project_id, "agent-session");
    let input = r#"{"command":"cargo test --workspace"}"#;
    {
        let store = Store::open(data.db_file()).expect("store");
        store.migrate().expect("migrate");
        let now: jiff::Timestamp = "2026-10-03T09:00:00Z".parse().expect("timestamp");
        store.upsert_project(&scope, now).expect("project");
        let session = new_session(
            session_id,
            scope.project_id,
            scope.workspace_id,
            AgentKind::ClaudeCode,
            repo.clone(),
            now,
            None,
        );
        store.ensure_session(&session).expect("session");
        let spool = RawSpool::new(data.raw());

        // Recorded the way the server records a call: each half into the
        // index and the transcript, and the call settled once both are in.
        // A second call never came back, and keeps its input.
        for (kind, call, body) in [
            (EventKind::ToolAttempt, "toolu_1", input.to_owned()),
            (
                EventKind::ToolUse,
                "toolu_1",
                format!("{input}\n→ test result: ok. 82 passed"),
            ),
            (
                EventKind::ToolAttempt,
                "toolu_2",
                r#"{"command":"cargo build"}"#.to_owned(),
            ),
        ] {
            let observation = new_observation(
                session_id,
                kind,
                tool(call),
                BoundedBody::truncating(body, 16 * 1024),
                now,
            );
            store.insert_observation(&observation).expect("observation");
            spool
                .append(&scope.scope, &session, &observation)
                .expect("transcript");
        }
        assert_eq!(
            store
                .settle_tool_calls(session_id, Some("toolu_1"))
                .expect("settle"),
            1
        );
    }

    let (clean, report) = run(anamnesis(&repo, &data_path).args(["reindex", "--check"]));
    assert!(
        clean,
        "the index with the input kept once is what its transcript rebuilds to: {report}"
    );

    // And from nothing but the transcript.
    for suffix in ["", "-wal", "-shm"] {
        let file = data.db_dir().join(format!("anamnesis.db{suffix}"));
        if file.exists() {
            std::fs::remove_file(&file).expect("remove the index");
        }
    }
    let (rebuilt, report) = run(anamnesis(&repo, &data_path).arg("reindex"));
    assert!(rebuilt, "reindex: {report}");

    let store = Store::open(data.db_file()).expect("store");
    let observations = store.observations(session_id).expect("observations");
    assert_eq!(observations.len(), 3, "every row came back: {report}");
    let attempt = |call: &str| {
        observations
            .iter()
            .find(|o| {
                o.kind == EventKind::ToolAttempt
                    && o.tool.as_ref().and_then(|t| t.call_id.as_deref()) == Some(call)
            })
            .map(|o| o.body.as_str().to_owned())
            .expect("the attempt")
    };
    assert_eq!(
        attempt("toolu_1"),
        "",
        "the rebuilt attempt repeats its completion again"
    );
    assert_eq!(
        attempt("toolu_2"),
        r#"{"command":"cargo build"}"#,
        "an attempt whose call never came back keeps its input"
    );
}
