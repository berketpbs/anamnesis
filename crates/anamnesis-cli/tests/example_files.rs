//! The example files outside the crates name only what exists.
//!
//! `docker/.env.example` offered `ANAMNESIS_DB`, `PORT`, `BIND`,
//! `STORAGE_TYPE` and `MCP_BEARER_TOKEN`, and nothing read any of them. A
//! variable that is not read does nothing without a word, and for the last one
//! that meant a server left open by somebody who had set a token. Nothing
//! checked the file against the code, because it is not code.

use std::path::{Path, PathBuf};

/// Read by something other than anamnesis's own sources.
const READ_ELSEWHERE: &[(&str, &str)] = &[
    ("RUST_LOG", "tracing_subscriber's default filter variable"),
    ("POSTGRES_PASSWORD", "docker-compose.yml's postgres profile"),
];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Every `.rs` file under `crates/` but this one, read whole. This one names
/// the stale variables it checks for, and would find them in itself.
fn sources(root: &Path) -> String {
    let this = Path::new(file!())
        .file_name()
        .expect("this file has a name")
        .to_owned();
    let mut text = String::new();
    let mut pending = vec![root.join("crates")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name() != Some(this.as_os_str())
            {
                text.push_str(&std::fs::read_to_string(&path).expect("read source"));
            }
        }
    }
    text
}

/// The variable names an env file sets or offers, commented out or not.
fn names(env_file: &str) -> Vec<String> {
    env_file
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_start_matches('#').trim();
            let (name, _) = line.split_once('=')?;
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            valid.then(|| name.to_owned())
        })
        .collect()
}

#[test]
fn every_variable_the_env_example_names_is_one_something_reads() {
    let root = repository();
    let example =
        std::fs::read_to_string(root.join("docker/.env.example")).expect("docker/.env.example");
    let sources = sources(&root);
    let compose = std::fs::read_to_string(root.join("docker-compose.yml")).expect("compose");

    let offered = names(&example);
    assert!(
        offered.len() >= 5,
        "the file was read as naming almost nothing: {offered:?}"
    );
    for name in offered {
        let read_here = sources.contains(&format!("\"{name}\""));
        let read_there = READ_ELSEWHERE.iter().any(|(known, _)| *known == name)
            && (name != "POSTGRES_PASSWORD" || compose.contains(&name));
        assert!(
            read_here || read_there,
            "docker/.env.example offers {name}, and nothing reads it"
        );
    }
}

/// The check has teeth: a name from the old file is caught.
#[test]
fn a_name_nothing_reads_is_caught() {
    let sources = sources(&repository());
    for stale in ["ANAMNESIS_DB", "MCP_BEARER_TOKEN", "STORAGE_TYPE"] {
        assert!(!sources.contains(&format!("\"{stale}\"")), "{stale}");
    }
    assert_eq!(
        names("# ANAMNESIS_TOKEN=\nRUST_LOG=info\nnot a line\n"),
        vec!["ANAMNESIS_TOKEN", "RUST_LOG"]
    );
}
