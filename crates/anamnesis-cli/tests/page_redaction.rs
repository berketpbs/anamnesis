//! CLI writes must never commit or index the unredacted page text.
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
        .env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-page-redaction")
        .env("ANAMNESIS_LLM_PROVIDER", "none")
        .env("ANAMNESIS_EMBED_ENABLED", "0");
    cmd
}

#[test]
fn cli_page_create_and_patch_leave_only_redacted_text_in_history_and_index() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let data = dir.path().join("data");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(
        repo.join(".anamnesis.toml"),
        "[scope]\nworkspace = \"test\"\nproject = \"privacy\"\n",
    )
    .unwrap();
    let secret = format!("ghp_{}", "b".repeat(36));
    let initialized = command(&repo, &data).arg("init").output().unwrap();
    assert!(initialized.status.success());
    let created = command(&repo, &data)
        .args([
            "write-page",
            "--path",
            "notes/safe.md",
            "--title",
            &secret,
            "--body",
            &secret,
            "--entity",
            &secret,
        ])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let scope = anamnesis_core::scope::resolve_scope(&repo).unwrap();
    let path = anamnesis_core::page::PagePath::parse("notes/safe.md").unwrap();
    let wiki = anamnesis_wiki::Wiki::open(data.join("wiki")).unwrap();
    let page = wiki.read_versioned_page(&scope.scope, &path).unwrap();
    let patched = command(&repo, &data)
        .args([
            "patch-page",
            "--path",
            "notes/safe.md",
            "--expected-revision",
            &page.revision,
            "--title",
            &secret,
            "--body",
            &secret,
            "--entity",
            &secret,
            "--abstract",
            &secret,
        ])
        .output()
        .unwrap();
    assert!(
        patched.status.success(),
        "{}",
        String::from_utf8_lossy(&patched.stderr)
    );
    let page = wiki.read_versioned_page(&scope.scope, &path).unwrap();
    let text =
        anamnesis_wiki::render_document(&page.parsed.frontmatter, &page.parsed.body).unwrap();
    assert!(!text.contains(&secret));
    let store = anamnesis_store::Store::open(data.join("db/anamnesis.db")).unwrap();
    let indexed: (String, String) = store
        .connection()
        .query_row(
            "SELECT title, body FROM pages WHERE path = ?1",
            [path.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!serde_json::to_string(&indexed).unwrap().contains(&secret));
    assert!(indexed.1.contains("[redacted"));
    let git = git2::Repository::open(data.join("wiki")).unwrap();
    let mut walk = git.revwalk().unwrap();
    walk.push_head().unwrap();
    let full_path = format!("{}/{}/{}", scope.scope.workspace, scope.scope.project, path);
    for oid in walk {
        let tree = git.find_commit(oid.unwrap()).unwrap().tree().unwrap();
        if let Ok(entry) = tree.get_path(Path::new(&full_path)) {
            let blob = git.find_blob(entry.id()).unwrap();
            assert!(
                !String::from_utf8_lossy(blob.content()).contains(&secret),
                "secret entered Git history"
            );
        }
    }
    let rejected = command(&repo, &data)
        .args([
            "write-page",
            "--path",
            &format!("notes/{secret}.md"),
            "--title",
            "safe",
            "--body",
            "safe",
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains(&secret));
    let check = command(&repo, &data)
        .args(["reindex", "--check"])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
