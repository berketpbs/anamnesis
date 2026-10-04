//! Source reads stay scoped, bounded and faithful across fresh CLI processes.
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::{BoundedBody, EventKind};
use anamnesis_core::session::AgentKind;
use anamnesis_store::{Store, new_observation, new_session};
use std::path::Path;
use std::process::Command;

fn command(repo: &Path, data: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anamnesis"));
    for (name, _) in std::env::vars() {
        if name.starts_with("ANAMNESIS_") || name.ends_with("_API_KEY") {
            cmd.env_remove(name);
        }
    }
    cmd.current_dir(repo)
        .arg("--data-dir")
        .arg(data)
        .env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-source-read")
        .env("ANAMNESIS_LLM_PROVIDER", "none")
        .env("ANAMNESIS_EMBED_ENABLED", "0");
    cmd
}

#[test]
fn fresh_process_reads_preserve_source_and_do_not_cross_projects() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let other = dir.path().join("other");
    let data = dir.path().join("data");
    for (path, name) in [(&repo, "source"), (&other, "other")] {
        std::fs::create_dir(path).unwrap();
        std::fs::write(
            path.join(".anamnesis.toml"),
            format!("[scope]\nworkspace = \"test\"\nproject = \"{name}\"\n"),
        )
        .unwrap();
    }
    assert!(
        command(&repo, &data)
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    let scope = anamnesis_core::scope::resolve_scope(&repo).unwrap();
    let other_scope = anamnesis_core::scope::resolve_scope(&other).unwrap();
    let store = Store::open(data.join("db/anamnesis.db")).unwrap();
    let now = jiff::Timestamp::now();
    store.upsert_project(&other_scope, now).unwrap();
    let id = SessionId::derive(scope.project_id, "writer");
    let empty = SessionId::derive(scope.project_id, "empty");
    for identity in [id, empty] {
        store
            .ensure_session(&new_session(
                identity,
                scope.project_id,
                scope.workspace_id,
                AgentKind::ClaudeCode,
                repo.clone(),
                now,
                None,
            ))
            .unwrap();
    }
    for index in 0..25 {
        store
            .insert_observation(&new_observation(
                id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating(format!("captured prompt {index}"), 100),
                now,
            ))
            .unwrap();
    }
    let secret = format!("ghp_{}", "c".repeat(36));
    let original = format!("captured assistant prefix {secret}");
    let event = new_observation(
        id,
        EventKind::AssistantMessage,
        None,
        BoundedBody::from_stored(original.clone(), true),
        now,
    );
    let event_id = event.id.to_string();
    store.insert_observation(&event).unwrap();
    drop(store);

    let first = command(&repo, &data)
        .args(["show-session", &id.to_string(), "--json"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), 20);
    assert_eq!(first["next_offset"], 20);
    let next = command(&repo, &data)
        .args(["show-session", &id.to_string(), "--offset", "20", "--json"])
        .output()
        .unwrap();
    let next: serde_json::Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["events"].as_array().unwrap().len(), 6);
    assert!(next["next_offset"].is_null());
    let message = command(&repo, &data)
        .args([
            "show-session",
            &id.to_string(),
            "--kind",
            "assistant-message",
            "--json",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(message.stdout).unwrap();
    assert!(!text.contains(&secret));
    let message: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(message["events"][0]["id"], event_id);
    assert_eq!(message["events"][0]["truncated"], true);
    assert_eq!(message["events"][0]["redacted_on_read"], true);
    assert!(
        message["events"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("captured assistant prefix ")
    );
    let unavailable = command(&repo, &data)
        .args(["show-session", &empty.to_string(), "--json"])
        .output()
        .unwrap();
    let unavailable: serde_json::Value = serde_json::from_slice(&unavailable.stdout).unwrap();
    assert_eq!(unavailable["source_available"], false);
    assert_eq!(unavailable["assistant_messages_available"], false);
    let denied = command(&other, &data)
        .args(["show-session", &id.to_string(), "--json"])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stdout).contains("captured"));
    let invalid = command(&repo, &data)
        .args(["show-session", &id.to_string(), "--kind", "invented"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let store = Store::open(data.join("db/anamnesis.db")).unwrap();
    assert_eq!(
        store
            .observations(id)
            .unwrap()
            .last()
            .unwrap()
            .body
            .as_str(),
        original
    );
}
