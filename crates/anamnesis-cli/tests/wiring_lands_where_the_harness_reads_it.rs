//! Where the four commands that write a harness's configuration put it, run
//! from a subdirectory of the project.
//!
//! A person deep in `crates/something/src` who runs `doctor` and does what it
//! says is the whole of this test. Anchored at the working directory, those
//! commands wrote `.claude/settings.local.json` into that subdirectory, where
//! no harness reads it; `doctor` called a project that was recording unwired,
//! and then called it healthy once the dead file existed. Identity has always
//! been resolved by walking up to the marker. So is this.

use std::path::Path;
use std::process::Command;

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
    command.env("ANAMNESIS_KEY_SERVICE", "anamnesis-test-wiring");
    command.current_dir(cwd);
    command.arg("--data-dir").arg(data);
    command
}

/// A project with a marker at its root and somewhere to stand well below it.
fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().to_path_buf();
    std::fs::write(
        root.join(".anamnesis.toml"),
        "[scope]\nproject = \"wiring-test\"\n",
    )
    .expect("the marker is written");
    let deep = root.join("crates").join("something").join("src");
    std::fs::create_dir_all(&deep).expect("the subdirectory is created");
    (dir, deep)
}

fn run(command: &mut Command) -> String {
    let out = command.output().expect("the command runs");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the command failed: {text}");
    text
}

#[test]
fn install_hooks_writes_the_settings_file_at_the_project_root() {
    let (dir, deep) = project();
    let root = dir.path();
    let data = root.join("data");

    // The dry run first, because what it names is what a person checks before
    // allowing the write. It named `.\.claude\settings.local.json` from both
    // directories, which is the same sentence for two different files.
    let printed = run(anamnesis(&deep, &data).arg("install-hooks"));
    let expected = root.join(".claude").join("settings.local.json");
    assert!(
        printed.contains(&expected.display().to_string()),
        "the dry run named something other than {}: {printed}",
        expected.display()
    );

    run(anamnesis(&deep, &data).args(["install-hooks", "--write"]));
    assert!(
        expected.exists(),
        "the settings file is not at the project root"
    );
    assert!(
        !deep.join(".claude").exists(),
        "a settings file was written into the subdirectory, where nothing reads it"
    );
}

#[test]
fn install_mcp_writes_the_registration_at_the_project_root() {
    let (dir, deep) = project();
    let root = dir.path();
    let data = root.join("data");

    run(anamnesis(&deep, &data).args(["install-mcp", "--write"]));
    assert!(
        root.join(".mcp.json").exists(),
        "the registration is not at the project root"
    );
    assert!(
        !deep.join(".mcp.json").exists(),
        "a registration was written into the subdirectory, where nothing reads it"
    );
}

#[test]
fn an_explicit_path_is_still_the_path() {
    let (dir, deep) = project();
    let root = dir.path();
    let data = root.join("data");
    let chosen = root.join("elsewhere").join("settings.json");
    std::fs::create_dir_all(chosen.parent().expect("a parent")).expect("the directory is created");

    run(anamnesis(&deep, &data)
        .args(["install-hooks", "--write", "--settings"])
        .arg(&chosen));
    assert!(chosen.exists(), "--settings was not honoured");
    assert!(
        !root.join(".claude").exists(),
        "the project root was written to as well as the path that was asked for"
    );
}

#[test]
fn uninstall_finds_what_install_wrote_from_the_same_subdirectory() {
    let (dir, deep) = project();
    let root = dir.path();
    let data = root.join("data");

    run(anamnesis(&deep, &data).args(["install-hooks", "--write"]));
    let printed = run(anamnesis(&deep, &data).arg("uninstall"));

    let expected = root.join(".claude").join("settings.local.json");
    assert!(
        printed.contains(&expected.display().to_string()),
        "uninstall did not find the file install-hooks had just written: {printed}"
    );
}

#[test]
fn doctor_reads_the_wiring_from_the_project_root() {
    let (dir, deep) = project();
    let root = dir.path();
    let data = root.join("data");

    run(anamnesis(&deep, &data).arg("init"));
    let before = run(anamnesis(&deep, &data).arg("doctor"));
    assert!(
        before.contains("no harness in this project is wired"),
        "an unwired project was not reported as one: {before}"
    );

    run(anamnesis(root, &data).args(["install-hooks", "--write"]));
    let after = run(anamnesis(&deep, &data).arg("doctor"));
    assert!(
        !after.contains("no harness in this project is wired"),
        "a project wired at its root is called unwired from a subdirectory: {after}"
    );
}
