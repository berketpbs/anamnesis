//! A renamed project's sessions still come back from its transcripts.
//!
//! `rename` moves the index to the new project id and the transcripts to the
//! new directory, but a transcript's header is written once and never again:
//! it keeps naming the project the session was recorded under. A rebuild that
//! trusted the header alone skipped every session from before the rename, so
//! the index that had just been moved was the only copy of them — and losing
//! it lost them.
//!
//! Driven through the binary, because the fault lived between two commands:
//! each was right about its own half.

use std::path::Path;
use std::process::Command;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::{BoundedBody, EventKind};
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
    command.env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-rename");
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

#[test]
fn a_renamed_projects_sessions_survive_losing_the_index() {
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

    // One session recorded the way the server records one: the row in the
    // index, the same observation in the transcript.
    let before = resolve_scope(&repo).expect("scope");
    let now: jiff::Timestamp = "2026-09-24T09:00:00Z".parse().expect("timestamp");
    {
        let store = Store::open(data.db_file()).expect("store");
        store.migrate().expect("migrate");
        store.upsert_project(&before, now).expect("project");
        let session = new_session(
            SessionId::derive(before.project_id, "agent-session"),
            before.project_id,
            before.workspace_id,
            AgentKind::ClaudeCode,
            repo.clone(),
            now,
            None,
        );
        store.ensure_session(&session).expect("session");
        let observation = new_observation(
            session.id,
            EventKind::UserPrompt,
            None,
            BoundedBody::truncating("amounts are Decimal, never float", 1024),
            now,
        );
        store.insert_observation(&observation).expect("observation");
        RawSpool::new(data.raw())
            .append(&before.scope, &session, &observation)
            .expect("transcript");
    }

    let (clean, report) = run(anamnesis(&repo, &data_path).args(["reindex", "--check"]));
    assert!(clean, "a freshly recorded session rebuilds: {report}");

    let (renamed, report) = run(anamnesis(&repo, &data_path).args(["rename", "gadget", "--apply"]));
    assert!(renamed, "rename: {report}");
    let after = resolve_scope(&repo).expect("scope");
    assert_ne!(
        after.project_id, before.project_id,
        "the marker was repinned"
    );

    let (clean, report) = run(anamnesis(&repo, &data_path).args(["reindex", "--check"]));
    assert!(
        clean,
        "after a rename the index is still what its sources rebuild to: {report}"
    );

    // The day it matters: the index is gone and only the sources remain.
    for suffix in ["", "-wal", "-shm"] {
        let file = data.db_dir().join(format!("anamnesis.db{suffix}"));
        if file.exists() {
            std::fs::remove_file(&file).expect("remove the index");
        }
    }
    let (rebuilt, report) = run(anamnesis(&repo, &data_path).arg("reindex"));
    assert!(rebuilt, "reindex: {report}");

    let store = Store::open(data.db_file()).expect("store");
    assert_eq!(
        store.session_count(after.project_id).expect("sessions"),
        1,
        "the session from before the rename came back: {report}"
    );
    let session = SessionId::derive(before.project_id, "agent-session");
    assert_eq!(store.observations(session).expect("observations").len(), 1);
}
