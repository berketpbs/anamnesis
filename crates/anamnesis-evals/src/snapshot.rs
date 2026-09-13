//! Pages taken from a copy of real memory, to ask questions of.
//!
//! The checked-in suites measure the retrieval code on corpora written for the
//! purpose, and that is what keeps them reproducible. It is also what keeps
//! them from saying anything about the memory a person actually has: pages the
//! consolidator wrote, in the languages they were written in, at the lengths
//! they grew to. A setting chosen on fixtures has to be checked on those before
//! it is anything but a candidate.
//!
//! This reads a wiki's pages from disk and turns them into the same
//! [`FixturePage`]s a suite declares, so everything that scores a suite —
//! `--compare`, `--streams`, `--embed` — scores a snapshot unchanged. The
//! corpus is still built in a throwaway directory. Nothing here opens the
//! index the pages came from or records a read against it, and nothing here
//! opens the wiki as a repository: the copy is left exactly as it was found.

use std::path::Path;

use anamnesis_core::scope::{ProjectName, Scope, WorkspaceName};

use crate::EvalError;
use crate::suite::FixturePage;

/// What was read from a snapshot, and what could not be.
#[derive(Debug, Clone)]
pub struct SnapshotPages {
    /// Every page that parsed, in path order.
    pub pages: Vec<FixturePage>,
    /// Files that looked like pages and did not parse, with the reason.
    ///
    /// Reported rather than fatal: a wiki belongs to the person who edits it,
    /// and one half-saved file should not stop a measurement of the other
    /// fifty. Reported rather than hidden, because a question whose answer is
    /// one of these can no longer be answered, and the validation that says
    /// so names the page and not why it is missing.
    pub unreadable: Vec<(String, String)>,
}

/// Read every page of one scope under `wiki_root`.
///
/// `wiki_root` is the directory a data directory calls `wiki/` — the one
/// holding `<workspace>/<project>/`.
pub fn pages_from_wiki(wiki_root: &Path, scope: &Scope) -> Result<SnapshotPages, EvalError> {
    let scope_root = wiki_root
        .join(scope.workspace.as_str())
        .join(scope.project.as_str());
    if !scope_root.is_dir() {
        return Err(EvalError::Corpus(format!(
            "{} holds no pages for {scope}",
            wiki_root.display()
        )));
    }

    let mut pages = Vec::new();
    let mut unreadable = Vec::new();
    for path in anamnesis_wiki::pages_under(&scope_root)? {
        let file = scope_root.join(path.as_str());
        let parsed = std::fs::read_to_string(&file)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                anamnesis_wiki::parse_document(path.as_str(), &text)
                    .map_err(|error| error.to_string())
            });
        match parsed {
            Ok(parsed) => {
                let front = parsed.frontmatter;
                pages.push(FixturePage {
                    path: path.as_str().to_owned(),
                    title: front.title,
                    body: parsed.body,
                    entities: front
                        .entities
                        .iter()
                        .map(|entity| entity.as_str().to_owned())
                        .collect(),
                    tier: front.tier.as_str().to_owned(),
                    canonical: front.canonical,
                    pinned: front.pinned,
                    supersedes: front
                        .supersedes
                        .map(|target| target.as_str().to_owned())
                        .unwrap_or_default(),
                    page_abstract: front.page_abstract.unwrap_or_default(),
                    status: front.status.as_str().to_owned(),
                });
            }
            Err(reason) => unreadable.push((path.as_str().to_owned(), reason)),
        }
    }

    if pages.is_empty() {
        return Err(EvalError::Corpus(format!(
            "no page under {} could be read",
            scope_root.display()
        )));
    }
    Ok(SnapshotPages { pages, unreadable })
}

/// Read `workspace/project`, as `--scope` takes it and `status` prints it.
pub fn parse_scope(text: &str) -> Result<Scope, EvalError> {
    let Some((workspace, project)) = text.trim().split_once('/') else {
        return Err(EvalError::Corpus(format!(
            "{text:?} is not a scope; expected workspace/project"
        )));
    };
    Ok(Scope {
        workspace: WorkspaceName::parse(workspace)?,
        project: ProjectName::parse(project)?,
    })
}

/// The scopes a wiki holds pages for, in sorted order, leaving out `_global`.
///
/// So a snapshot of a memory with one project needs no `--scope`, and one with
/// several can say which it has. `_global` is left out because it is shared by
/// every project in a workspace rather than being one of them.
pub fn scopes_in_wiki(wiki_root: &Path) -> Result<Vec<Scope>, EvalError> {
    let mut found = Vec::new();
    let read = |dir: &Path| {
        std::fs::read_dir(dir).map_err(|error| {
            EvalError::Corpus(format!("could not read {}: {error}", dir.display()))
        })
    };
    for workspace in read(wiki_root)? {
        let workspace = workspace.map_err(|error| EvalError::Corpus(error.to_string()))?;
        let name = workspace.file_name().to_string_lossy().into_owned();
        if !workspace.path().is_dir() || name.starts_with('.') {
            continue;
        }
        for project in read(&workspace.path())? {
            let project = project.map_err(|error| EvalError::Corpus(error.to_string()))?;
            let project_name = project.file_name().to_string_lossy().into_owned();
            if !project.path().is_dir() || project_name == "_global" {
                continue;
            }
            if let Ok(scope) = parse_scope(&format!("{name}/{project_name}")) {
                found.push(scope);
            }
        }
    }
    found.sort_by_key(ToString::to_string);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::Suite;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    fn snapshot() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let wiki = dir.path();
        write(
            wiki,
            "default/demo/decisions/0001-sqlite.md",
            "---\ntitle: Why SQLite\ntier: semantic\nstatus: active\npinned: false\n\
             canonical: true\nsupersedes: null\nsalience: 1.0\nentities:\n- SQLite\n\
             expires_at: null\n---\n\nOne file, one index.\n",
        );
        write(
            wiki,
            "default/demo/decisions/0000-postgres.md",
            "---\ntitle: Why Postgres\ntier: semantic\nstatus: superseded\npinned: false\n\
             canonical: false\nsupersedes: null\nsalience: 1.0\nentities: []\n\
             expires_at: null\n---\n\nA server to run.\n",
        );
        write(
            wiki,
            "default/demo/notes/half-saved.md",
            "no frontmatter here",
        );
        write(wiki, "default/_global/shared.md", "---\ntitle: x\n---\n");
        write(wiki, "default/other/a.md", "---\ntitle: a\n---\n\nbody\n");
        dir
    }

    #[test]
    fn a_page_keeps_what_its_frontmatter_says_and_a_broken_one_is_named() {
        let dir = snapshot();
        let scope: Scope = parse_scope("default/demo").expect("scope");

        let read = pages_from_wiki(dir.path(), &scope).expect("pages");

        let paths: Vec<&str> = read.pages.iter().map(|page| page.path.as_str()).collect();
        assert_eq!(
            paths,
            ["decisions/0000-postgres.md", "decisions/0001-sqlite.md"]
        );
        let sqlite = &read.pages[1];
        assert_eq!(sqlite.tier, "semantic");
        assert!(sqlite.canonical);
        assert_eq!(sqlite.entities, ["SQLite"]);
        assert_eq!(
            read.pages[0].status, "superseded",
            "a page real memory withholds stays withheld"
        );
        assert_eq!(read.unreadable.len(), 1);
        assert_eq!(read.unreadable[0].0, "notes/half-saved.md");
    }

    #[test]
    fn the_scopes_are_listed_without_the_shared_one() {
        let dir = snapshot();
        let scopes: Vec<String> = scopes_in_wiki(dir.path())
            .expect("scopes")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(scopes, ["default/demo", "default/other"]);
    }

    #[test]
    fn a_scope_with_nothing_on_disk_is_refused_by_name() {
        let dir = snapshot();
        let scope: Scope = parse_scope("default/missing").expect("scope");
        let error = pages_from_wiki(dir.path(), &scope).expect_err("nothing there");
        assert!(error.to_string().contains("default/missing"), "{error}");
    }

    /// The whole path, as `anamnesis eval --pages-from` takes it: questions
    /// from one file, pages from a snapshot, validated against each other.
    #[test]
    fn questions_are_asked_of_a_snapshot_and_validated_against_it() {
        let dir = snapshot();
        let scope: Scope = parse_scope("default/demo").expect("scope");
        let pages = pages_from_wiki(dir.path(), &scope).expect("pages").pages;

        let questions = r#"
name = "demo-live"
description = "asked of a snapshot"

[[case]]
query = "why one file"
relevant = ["decisions/0001-sqlite.md"]
"#;
        let suite = Suite::from_questions(questions, pages.clone()).expect("a suite");
        let report =
            crate::run(&suite, "2026-01-01T00:00:00Z".parse().expect("now")).expect("scored");
        assert_eq!(report.cases.len(), 1);

        let missing = r#"
name = "demo-live"
description = "asks for a page the snapshot lacks"

[[case]]
query = "why"
relevant = ["notes/half-saved.md"]
"#;
        assert!(
            Suite::from_questions(missing, pages.clone())
                .expect_err("unanswerable")
                .to_string()
                .contains("notes/half-saved.md")
        );

        let with_pages = r#"
name = "demo-live"
description = "brings its own"

[[page]]
path = "a.md"
title = "a"
body = "b"

[[case]]
query = "a"
relevant = ["a.md"]
"#;
        assert!(
            Suite::from_questions(with_pages, pages)
                .expect_err("two corpora")
                .to_string()
                .contains("must carry none")
        );
    }
}
