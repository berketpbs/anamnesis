//! A portable file preserves scoped facts without taking the next agent's note.
use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::SessionId;
use anamnesis_core::observation::{BoundedBody, EventKind};
use anamnesis_core::page::{Frontmatter, PagePath};
use anamnesis_core::session::AgentKind;
use anamnesis_core::workstream::{Workstream, WorkstreamSlug};
use anamnesis_store::{Store, new_handoff, new_observation, new_session};
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
        .env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-brief")
        .env("ANAMNESIS_LLM_PROVIDER", "none")
        .env("ANAMNESIS_EMBED_ENABLED", "0");
    cmd
}

#[test]
fn portable_brief_keeps_decisions_rejections_sources_and_pending_slot() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let data = dir.path().join("data");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join(".anamnesis.toml"),
        "[scope]\nworkspace = \"test\"\nproject = \"brief\"\n",
    )
    .unwrap();
    assert!(
        command(&repo, &data)
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    let scope = anamnesis_core::scope::resolve_scope(&repo).unwrap();
    let store = Store::open(data.join("db/anamnesis.db")).unwrap();
    let wiki = anamnesis_wiki::Wiki::open(data.join("wiki")).unwrap();
    let now = jiff::Timestamp::now();
    let id = SessionId::derive(scope.project_id, "writer");
    store
        .ensure_session(&new_session(
            id,
            scope.project_id,
            scope.workspace_id,
            AgentKind::ClaudeCode,
            repo.clone(),
            now,
            None,
        ))
        .unwrap();
    let secret = format!("ghp_{}", "e".repeat(36));
    store
        .insert_observation(&new_observation(
            id,
            EventKind::AssistantMessage,
            None,
            BoundedBody::truncating(format!("{} {} tail", "ş".repeat(1995), secret), 8000),
            now,
        ))
        .unwrap();
    let note =
        "Accepted SQLite. Rejected Redis: restore lost writes. Next step: verify backup restore.";
    store
        .record_handoff(&new_handoff(
            scope.project_id,
            id,
            Slot::shared(),
            note,
            now,
        ))
        .unwrap();
    for (name, body, supersedes, expired) in [
        ("decisions/old.md", "OBSOLETE_POLICY", None, false),
        (
            "decisions/new.md",
            "Accepted SQLite; rejected Redis because restore lost writes.",
            Some("decisions/old.md"),
            false,
        ),
        ("decisions/expired.md", "EXPIRED_POLICY", None, true),
    ] {
        let mut fm = Frontmatter::new(name, vec![]).unwrap();
        fm.supersedes = supersedes.map(|p| PagePath::parse(p).unwrap());
        fm.expires_at = expired.then(|| "2000-01-01T00:00:00Z".parse().unwrap());
        let page = anamnesis_wiki::page(scope.project_id, PagePath::parse(name).unwrap(), fm, body);
        wiki.write_page(&scope.scope, &page, "fixture decision")
            .unwrap();
        store.upsert_page(&page, now).unwrap();
    }
    let head = git2::Repository::open(data.join("wiki"))
        .unwrap()
        .head()
        .unwrap()
        .target();
    let out = repo.join("durum.md");
    let result = command(&repo, &data)
        .arg("brief")
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains(note));
    assert!(text.contains(&id.to_string()));
    assert!(text.contains("decisions/new.md"));
    assert!(!text.contains("OBSOLETE_POLICY"));
    assert!(!text.contains("EXPIRED_POLICY"));
    assert!(
        !text.contains("ghp_"),
        "redact before clipping a key at the excerpt boundary"
    );
    assert_eq!(
        store
            .peek_handoff(scope.project_id, &Slot::shared())
            .unwrap()
            .as_deref(),
        Some(note)
    );
    assert_eq!(
        git2::Repository::open(data.join("wiki"))
            .unwrap()
            .head()
            .unwrap()
            .target(),
        head
    );
    let refused = command(&repo, &data)
        .arg("brief")
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
    let rewritten = command(&repo, &data)
        .args(["brief", "--force", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(rewritten.status.success());
    let reader = SessionId::derive(scope.project_id, "reader");
    store
        .ensure_session(&new_session(
            reader,
            scope.project_id,
            scope.workspace_id,
            AgentKind::Codex,
            repo.clone(),
            now,
            None,
        ))
        .unwrap();
    assert_eq!(
        store
            .claim_handoff(scope.project_id, reader, &Slot::shared(), now)
            .unwrap()
            .as_deref(),
        Some(note)
    );
}

#[test]
fn named_workstreams_do_not_copy_each_others_checkpoint_or_decision() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let data = dir.path().join("data");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join(".anamnesis.toml"),
        "[scope]\nworkspace = \"test\"\nproject = \"parallel\"\n",
    )
    .unwrap();
    assert!(
        command(&repo, &data)
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    let scope = anamnesis_core::scope::resolve_scope(&repo).unwrap();
    let store = Store::open(data.join("db/anamnesis.db")).unwrap();
    let wiki = anamnesis_wiki::Wiki::open(data.join("wiki")).unwrap();
    let now = jiff::Timestamp::now();
    for name in ["alpha", "beta"] {
        let stream = Workstream::new(
            scope.project_id,
            WorkstreamSlug::parse(name).unwrap(),
            name,
            now,
        );
        store.upsert_workstream(&stream).unwrap();
        let id = SessionId::derive(scope.project_id, name);
        let mut session = new_session(
            id,
            scope.project_id,
            scope.workspace_id,
            AgentKind::Codex,
            repo.clone(),
            now,
            None,
        );
        session.workstream_id = Some(stream.id);
        store.ensure_session(&session).unwrap();
        store
            .insert_observation(&new_observation(
                id,
                EventKind::AssistantMessage,
                None,
                BoundedBody::truncating(format!("checkpoint-{name}"), 200),
                now,
            ))
            .unwrap();
        let mut fm = Frontmatter::new(name, vec![]).unwrap();
        fm.session = Some(id);
        let page = anamnesis_wiki::page(
            scope.project_id,
            PagePath::parse(&format!("decisions/{name}.md")).unwrap(),
            fm,
            format!("decision-{name}"),
        );
        wiki.write_page(&scope.scope, &page, "fixture").unwrap();
        store.upsert_page(&page, now).unwrap();
    }
    let out = repo.join("alpha.md");
    assert!(
        command(&repo, &data)
            .args(["brief", "--workstream", "alpha", "--out"])
            .arg(&out)
            .output()
            .unwrap()
            .status
            .success()
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("checkpoint-alpha"));
    assert!(text.contains("decision-alpha"));
    assert!(!text.contains("checkpoint-beta"));
    assert!(!text.contains("decision-beta"));
}
