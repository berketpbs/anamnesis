//! Building the corpus a suite is scored against.
//!
//! Written through the same three calls the live path uses — the wiki, then
//! the index, then entities and links — because an eval that populated the
//! index directly would be scoring a corpus no running system could produce.
//! The frontmatter round trip, the wikilink extraction, and the FTS triggers
//! are all part of what is under test.
//!
//! The corpus is always a throwaway directory, never anyone's real memory.
//! That is not only about determinism: [`anamnesis_store::Store::query_pages`]
//! records an access for every page it returns, and the decay sweep reads
//! exactly that number to decide what is worth keeping. Pointed at a real
//! index, a hundred eval queries would look like a hundred afternoons of
//! someone finding those pages useful.

use anamnesis_core::ids::ProjectId;
use anamnesis_core::page::{Frontmatter, Page};
use anamnesis_core::scope::{ProjectName, Scope, resolve_scope};
use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

use crate::EvalError;
use crate::suite::Suite;

/// Workspace every corpus is built under.
const WORKSPACE: &str = "default";

/// A built corpus: an index, a wiki, and the project they belong to.
///
/// Holds its own temporary directory. Dropping it deletes both.
pub struct Corpus {
    /// Deleted when this struct goes.
    _dir: tempfile::TempDir,
    /// The index the queries run against.
    pub store: Store,
    /// The project the pages belong to.
    pub project_id: ProjectId,
    /// The scope, for locating pages in the wiki.
    pub scope: Scope,
}

impl Corpus {
    /// Write a suite's pages into a fresh index and wiki.
    pub fn build(suite: &Suite, now: Timestamp) -> Result<Self, EvalError> {
        let dir = tempfile::tempdir().map_err(|error| {
            EvalError::Corpus(format!("could not create a temporary directory: {error}"))
        })?;

        // Resolved from a marker file rather than assembled by hand, so the
        // project identifier the eval writes under is derived the same way a
        // real project's is. A hand-built scope would be the one part of the
        // corpus that no running system produces.
        let project = ProjectName::sanitized(&suite.name)?;
        std::fs::write(
            dir.path().join(anamnesis_core::scope::MARKER_FILE),
            format!("[scope]\nworkspace = \"{WORKSPACE}\"\nproject = \"{project}\"\n"),
        )
        .map_err(|error| EvalError::Corpus(format!("could not write the marker: {error}")))?;
        let resolved = resolve_scope(dir.path())?;

        let store = Store::open(dir.path().join("index.db"))?;
        store.migrate()?;
        let wiki = Wiki::open(dir.path().join("wiki"))?;

        store.upsert_project(&resolved, now)?;
        let project_id = resolved.project_id;
        let scope = resolved.scope.clone();

        for fixture in &suite.pages {
            let mut frontmatter =
                Frontmatter::new(fixture.title.clone(), fixture.parsed_entities()?)?;
            frontmatter.tier = fixture.parsed_tier()?;
            frontmatter.canonical = fixture.canonical;
            frontmatter.pinned = fixture.pinned;

            let mut page = Page::new(
                project_id,
                fixture.page_path()?,
                frontmatter,
                fixture.body.clone(),
            );
            let commit = wiki.write_page(&scope, &page, &format!("eval: {}", fixture.path))?;
            page.git_commit = Some(commit);

            store.upsert_page(&page, now)?;
            store.set_page_entities(project_id, page.id, &page.frontmatter.entities)?;
            store.set_page_links(
                project_id,
                page.id,
                &anamnesis_wiki::extract_links(&page.body),
            )?;
        }

        Ok(Self {
            _dir: dir,
            store,
            project_id,
            scope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::Suite;

    const SUITE: &str = r#"
name = "corpus-test"
description = "two pages, one linking to the other"

[[page]]
path = "decisions/0001-sqlite.md"
title = "Why SQLite"
tier = "semantic"
entities = ["SQLite"]
body = "Chosen for the single-file index. See [[notes/benchmarks.md]]."

[[page]]
path = "notes/benchmarks.md"
title = "Benchmarks"
body = "Numbers behind the choice."

[[case]]
query = "which database"
relevant = ["decisions/0001-sqlite.md"]
"#;

    fn now() -> Timestamp {
        "2026-08-28T09:00:00Z".parse().expect("timestamp")
    }

    /// The corpus is built the way the live path builds one, so everything the
    /// live path derives has to be there: the row, its entities, and the link
    /// it resolved to another page.
    #[test]
    fn a_built_corpus_carries_what_retrieval_reads() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let corpus = Corpus::build(&suite, now()).expect("build");

        let hits = corpus
            .store
            .query_pages(corpus.project_id, "sqlite", 5, now(), None)
            .expect("query");
        assert!(
            hits.iter()
                .any(|hit| hit.path.as_str() == "decisions/0001-sqlite.md"),
            "the page is not reachable at all: {hits:?}"
        );

        // The link stream needs `page_links` populated, which only happens
        // because the body went through the same extraction the server uses.
        let hits = corpus
            .store
            .query_pages(corpus.project_id, "benchmarks", 5, now(), None)
            .expect("query");
        assert!(
            hits.len() >= 2,
            "the link neighbour was not reached: {hits:?}"
        );
    }

    /// Every run starts from nothing. Two corpora built from one suite must
    /// not be able to see each other's pages, or a score would depend on what
    /// ran before it.
    #[test]
    fn each_corpus_is_its_own() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let first = Corpus::build(&suite, now()).expect("build");
        let second = Corpus::build(&suite, now()).expect("build");

        assert_eq!(
            first.store.page_count(first.project_id).expect("count"),
            second.store.page_count(second.project_id).expect("count"),
        );
        assert_eq!(first.store.page_count(first.project_id).expect("count"), 2);
    }
}
