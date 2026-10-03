//! Preview never removes rows; apply protects live and configured model choices.
use anamnesis_core::page::{Frontmatter, Page, PagePath};
use anamnesis_store::Store;
use std::io::{Read, Write};
use std::net::TcpListener;
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
        .env("ANAMNESIS_LLM_PROVIDER", "none")
        .env("ANAMNESIS_EMBED_ENABLED", "0")
        .env("ANAMNESIS_EMBED_MODEL", "configured")
        .env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-prune");
    cmd
}

#[test]
fn cli_preview_offline_refusal_and_confirmed_apply_preserve_active_rows() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let data = dir.path().join("data");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join(".anamnesis.toml"),
        "[scope]\nworkspace = \"test\"\nproject = \"prune\"\n",
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
    let page = Page::new(
        scope.project_id,
        PagePath::parse("notes/page.md").unwrap(),
        Frontmatter::new("page", vec![]).unwrap(),
        "preserve",
    );
    store.upsert_page(&page, jiff::Timestamp::now()).unwrap();
    for model in ["configured", "live", "old"] {
        store.set_page_embedding(page.id, model, &[1.]).unwrap();
    }
    let offline = "http://127.0.0.1:1";
    let preview = command(&repo, &data)
        .args(["vectors", "prune", "--server", offline])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("Preview only"));
    assert_eq!(store.vector_models(scope.project_id).unwrap().len(), 3);
    let refused = command(&repo, &data)
        .args([
            "vectors", "prune", "--model", "old", "--apply", "--server", offline,
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(store.vector_models(scope.project_id).unwrap().len(), 3);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        // Apply asks twice: first for preview/protection, then before mutation.
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 4096];
            let received = stream.read(&mut buffer).unwrap();
            assert!(received > 0);
            let body = r#"{"embedding":"live"}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let applied = command(&repo, &data)
        .args(["vectors", "prune", "--apply", "--server", &url])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let remaining: Vec<_> = store
        .vector_models(scope.project_id)
        .unwrap()
        .into_iter()
        .map(|row| row.model)
        .collect();
    assert_eq!(remaining, ["configured", "live"]);
    store.set_page_embedding(page.id, "old", &[1.]).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let changed_url = format!("http://{}", listener.local_addr().unwrap());
    let changed = std::thread::spawn(move || {
        for model in ["live", "old"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 4096];
            assert!(stream.read(&mut buffer).unwrap() > 0);
            let body = format!(r#"{{"embedding":"{model}"}}"#);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let refused_change = command(&repo, &data)
        .args(["vectors", "prune", "--apply", "--server", &changed_url])
        .output()
        .unwrap();
    changed.join().unwrap();
    assert!(!refused_change.status.success());
    assert!(String::from_utf8_lossy(&refused_change.stderr).contains("selection changed"));
    assert_eq!(store.vector_models(scope.project_id).unwrap().len(), 3);
    let protected = command(&repo, &data)
        .args([
            "vectors",
            "prune",
            "--model",
            "configured",
            "--apply",
            "--server",
            offline,
        ])
        .output()
        .unwrap();
    assert!(!protected.status.success());
}
