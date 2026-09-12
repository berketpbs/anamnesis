//! Properties of retrieval, over queries nobody wrote down.
//!
//! A query arrives from an agent, a person, or a pasted log, and every word of
//! it is quoted into a full-text expression and bound into SQL. The examples in
//! `query.rs` cover the questions somebody thought to ask. These cover the text
//! nobody would think to type: FTS5 operators, unbalanced quotes, column
//! filters, scripts that fold case unevenly, and very long input.

use anamnesis_core::ids::ProjectId;
use anamnesis_core::page::{Entity, Frontmatter, Page, PagePath, Tier};
use anamnesis_core::retrieval::Tuning;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;
use jiff::Timestamp;
use proptest::prelude::*;

fn now() -> Timestamp {
    "2026-09-12T09:00:00Z".parse().expect("timestamp")
}

/// A small memory worth searching: a few pages with bodies, entities and one
/// link, indexed the way the server indexes them.
fn memory() -> (tempfile::TempDir, Store, ProjectId) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(".anamnesis.toml"),
        "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
    )
    .expect("marker");
    let scope = resolve_scope(dir.path()).expect("scope");

    let store = Store::open_in_memory().expect("open");
    store.migrate().expect("migrate");
    store.upsert_project(&scope, now()).expect("project");
    let project = scope.project_id;

    let pages = [
        (
            "decisions/0001-sqlite.md",
            "Why SQLite",
            "One file, no server. See [[notes/wal.md]].",
            vec!["SQLite"],
        ),
        (
            "notes/wal.md",
            "Write-ahead logging",
            "The -wal file holds what was committed most recently.",
            vec!["WAL"],
        ),
        (
            "gotchas/windows-bom.md",
            "PowerShell writes a byte order mark",
            "Set-Content -Encoding utf8 prepends one on 5.1; İstanbul, straße, Ωmega.",
            vec!["Windows BOM"],
        ),
        (
            "sessions/2026-09-01-abc.md",
            "Session: quoting \"everything\"",
            "column:filter NEAR(a b) \"unbalanced * ^prefix -negation OR AND NOT",
            vec![],
        ),
    ];
    for (path, title, body, entities) in pages {
        let entities: Vec<Entity> = entities
            .into_iter()
            .map(|name| Entity::parse(name).expect("entity"))
            .collect();
        let mut frontmatter = Frontmatter::new(title, entities).expect("frontmatter");
        frontmatter.tier = Tier::Semantic;
        let page = Page::new(
            project,
            PagePath::parse(path).expect("path"),
            frontmatter,
            body,
        );
        let links: Vec<String> = if body.contains("[[notes/wal.md]]") {
            vec!["notes/wal.md".to_owned()]
        } else {
            Vec::new()
        };
        store
            .index_page(project, &page, &links, None, now())
            .expect("index");
    }

    (dir, store, project)
}

/// Query text, weighted toward what full-text syntax and SQL find interesting.
fn hostile_query() -> impl Strategy<Value = String> {
    let word = prop_oneof![
        3 => "[a-z]{1,10}",
        1 => prop::sample::select(vec![
            "\"", "\"\"", "'", "*", "^", ":", "-", "(", ")", "NEAR", "OR", "AND", "NOT",
            "title:", "body:", "{", "}", "%", "_", "\\", ";", "--", "/*", "*/", "\u{0}",
        ])
        .prop_map(str::to_owned),
        1 => "\\PC{1,6}",
        1 => prop::sample::select(vec!["sqlite", "wal", "windows", "bom", "istanbul", "İSTANBUL", "STRASSE"])
            .prop_map(str::to_owned),
    ];
    prop::collection::vec(word, 0..12).prop_map(|words| words.join(" "))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever is typed, a query answers — with pages or with none, never
    /// with an error, and never with more than it was asked for or with the
    /// same page twice.
    #[test]
    fn any_query_answers_within_its_limit(query in hostile_query(), limit in 0usize..8) {
        let (_dir, store, project) = memory();

        let hits = store
            .query_pages(project, &query, limit, now(), None)
            .map_err(|error| TestCaseError::fail(format!("{query:?}: {error}")))?;

        prop_assert!(hits.len() <= limit);
        let mut seen = std::collections::HashSet::new();
        for hit in &hits {
            prop_assert!(seen.insert(hit.page_id), "{:?} returned twice for {query:?}", hit.path);
            prop_assert!(hit.score.is_finite() && hit.score > 0.0, "score {} for {query:?}", hit.score);
        }
        for pair in hits.windows(2) {
            prop_assert!(pair[0].score >= pair[1].score, "not ranked best first for {query:?}");
        }
    }

    /// The breakdown `explain` reads comes from the same words, and has to
    /// survive the same text.
    #[test]
    fn any_query_can_be_explained(query in hostile_query()) {
        let (_dir, store, project) = memory();
        store
            .query_streams(project, &query, 5, None, &Tuning::default())
            .map_err(|error| TestCaseError::fail(format!("{query:?}: {error}")))?;
    }

    /// What a hit carries of its page is the start of the body, cut on a
    /// character boundary, whatever the body is written in.
    #[test]
    fn a_snippet_is_the_start_of_the_page(body in "\\PC{0,700}") {
        let (_dir, store, project) = memory();
        let text = format!("zyzzyva {body}");
        let mut frontmatter = Frontmatter::new("Probe", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Semantic;
        let page = Page::new(
            project,
            PagePath::parse("notes/probe.md").expect("path"),
            frontmatter,
            text.as_str(),
        );
        store.index_page(project, &page, &[], None, now()).expect("index");

        let hits = store.query_pages(project, "zyzzyva", 1, now(), None).expect("query");
        prop_assert_eq!(hits.len(), 1);
        let snippet = &hits[0].snippet;
        let kept = snippet.strip_suffix('…').unwrap_or(snippet);
        prop_assert!(
            text.trim().starts_with(kept),
            "{snippet:?} is not the start of the body"
        );
        prop_assert!(kept.chars().count() <= 240, "{} characters", kept.chars().count());
    }

    /// Asking the same question twice gets the same ranking. Access counts
    /// change between the two, and ranking must not read them.
    #[test]
    fn the_same_query_ranks_the_same_twice(query in hostile_query()) {
        let (_dir, store, project) = memory();
        let first: Vec<_> = store
            .query_pages(project, &query, 5, now(), None)
            .expect("first")
            .into_iter()
            .map(|hit| hit.page_id)
            .collect();
        let second: Vec<_> = store
            .query_pages(project, &query, 5, now(), None)
            .expect("second")
            .into_iter()
            .map(|hit| hit.page_id)
            .collect();
        prop_assert_eq!(first, second);
    }
}
