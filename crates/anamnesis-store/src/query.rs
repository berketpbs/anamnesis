//! Retrieval: fusing full-text search, entity matching, and link-neighbour
//! expansion into one ranked list of pages.
//!
//! Three independent SQL queries each produce a ranked stream of page ids;
//! turning several rankings into one is [`anamnesis_core::retrieval`], which
//! is pure and knows nothing about SQL. Everything here is the SQL that
//! produces the streams, plus the bookkeeping (authority multiplier, access
//! recording) that surrounds fusing them.
//!
//! What's deliberately absent: a vector-cosine stream. That needs an
//! embedding pipeline this crate does not have yet, so relevance today comes
//! from three of the four signals the design calls for. A hit that all three
//! agree on is no less real for the fourth signal's absence.

use std::collections::HashMap;

use anamnesis_core::ids::{PageId, ProjectId};
use anamnesis_core::page::{Entity, PagePath, PageStatus, Tier};
use anamnesis_core::retrieval::{RRF_K, authority_multiplier, fuse_and_rank};
use jiff::Timestamp;
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::{Result, Store};

/// How many candidates each individual stream contributes before fusion.
const STREAM_CANDIDATES: usize = 30;

/// Longest snippet returned with a hit, in characters.
const SNIPPET_LEN: usize = 240;

/// A page returned by [`Store::query_pages`], ranked by fused relevance.
#[derive(Debug, Clone, PartialEq)]
pub struct PageHit {
    /// Identifies the page.
    pub page_id: PageId,
    /// Project-relative path.
    pub path: PagePath,
    /// Title from frontmatter.
    pub title: String,
    /// Temporal tier.
    pub tier: Tier,
    /// Trust status.
    pub status: PageStatus,
    /// Exempt from decay.
    pub pinned: bool,
    /// Declared authoritative on its subject.
    pub canonical: bool,
    /// Fused relevance score, after the authority multiplier.
    pub score: f64,
    /// Leading slice of the body, for a caller deciding whether to read more.
    pub snippet: String,
}

/// Row shape shared by every stream before fusion picks winners.
struct PageRow {
    path: PagePath,
    title: String,
    body: String,
    tier: Tier,
    status: PageStatus,
    pinned: bool,
    canonical: bool,
}

impl Store {
    /// Search a project's pages, fusing full-text, entity, and link-neighbour
    /// relevance into one ranking.
    ///
    /// Every returned page has its access statistics bumped — `memory_query`
    /// finding a page is exactly the "proven useful" signal the decay formula
    /// is built to reward.
    pub fn query_pages(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        now: Timestamp,
    ) -> Result<Vec<PageHit>> {
        let tokens = tokenize(query);
        if tokens.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let fts = self.fts_stream(project_id, &tokens, STREAM_CANDIDATES)?;
        let entity = self.entity_stream(project_id, &tokens, STREAM_CANDIDATES)?;

        let mut seeds: Vec<PageId> = Vec::with_capacity(fts.len() + entity.len());
        for id in fts.iter().chain(entity.iter()) {
            if !seeds.contains(id) {
                seeds.push(*id);
            }
        }
        seeds.truncate(STREAM_CANDIDATES);
        let links = self.link_stream(project_id, &seeds, STREAM_CANDIDATES)?;

        let fused = fuse_and_rank(&[fts, entity, links], RRF_K);
        if fused.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<PageId> = fused.iter().map(|(id, _)| *id).collect();
        let rows = self.load_page_rows(&ids)?;

        let mut hits: Vec<PageHit> = fused
            .into_iter()
            .filter_map(|(id, score)| {
                let row = rows.get(&id)?;
                let adjusted = score
                    * authority_multiplier(row.pinned, row.canonical, row.path.is_authoritative());
                Some(PageHit {
                    page_id: id,
                    path: row.path.clone(),
                    title: row.title.clone(),
                    tier: row.tier,
                    status: row.status,
                    pinned: row.pinned,
                    canonical: row.canonical,
                    score: adjusted,
                    snippet: snippet_of(&row.body),
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);

        for hit in &hits {
            self.record_access(hit.page_id, now)?;
        }

        Ok(hits)
    }

    /// Bump a page's access statistics, as `memory_query` does for everything
    /// it returns.
    pub fn record_access(&self, page_id: PageId, now: Timestamp) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE pages SET access_count = access_count + 1, last_accessed_at = ?2
             WHERE id = ?1",
            params![page_id.to_string(), now.to_string()],
        )?;
        Ok(())
    }

    /// Replace a page's declared entities, creating any new to the project.
    ///
    /// Deleting and reinserting rather than diffing keeps this simple, and a
    /// page's entity list is short enough (capped at ten, see
    /// [`anamnesis_core::page::MAX_ENTITIES`]) that the cost never matters.
    pub fn set_page_entities(
        &self,
        project_id: ProjectId,
        page_id: PageId,
        entities: &[Entity],
    ) -> Result<()> {
        let mut conn = self.connection();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM page_entities WHERE page_id = ?1",
            params![page_id.to_string()],
        )?;
        for entity in entities {
            tx.execute(
                "INSERT INTO entities (project_id, name) VALUES (?1, ?2)
                 ON CONFLICT (project_id, name) DO NOTHING",
                params![project_id.to_string(), entity.as_str()],
            )?;
            let entity_id: i64 = tx.query_row(
                "SELECT id FROM entities WHERE project_id = ?1 AND name = ?2",
                params![project_id.to_string(), entity.as_str()],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO page_entities (page_id, entity_id) VALUES (?1, ?2)
                 ON CONFLICT (page_id, entity_id) DO NOTHING",
                params![page_id.to_string(), entity_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace a page's outgoing wikilinks, resolving each target to an
    /// existing page in the same project when one exists.
    ///
    /// An unresolved target is not an error: `[[some-page]]` pointing at a
    /// page that does not exist yet is intent, not a mistake, and it resolves
    /// by itself the moment that page is written (the next call to this
    /// method, on whichever page names it, links it up).
    pub fn set_page_links(
        &self,
        project_id: ProjectId,
        page_id: PageId,
        targets: &[String],
    ) -> Result<()> {
        let mut conn = self.connection();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM page_links WHERE from_page_id = ?1",
            params![page_id.to_string()],
        )?;
        for target in targets {
            let resolved: Option<String> = tx
                .query_row(
                    "SELECT id FROM pages
                     WHERE project_id = ?1 AND is_latest = 1
                       AND (path = ?2 OR path = ?2 || '.md')",
                    params![project_id.to_string(), target],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT INTO page_links (from_page_id, to_target, to_page_id, to_project_id)
                 VALUES (?1, ?2, ?3, NULL)
                 ON CONFLICT (from_page_id, to_target) DO UPDATE SET to_page_id = excluded.to_page_id",
                params![page_id.to_string(), target, resolved],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Full-text stream: pages whose indexed content matches any query token,
    /// ranked by SQLite's bm25 (lower is a better match).
    fn fts_stream(&self, project_id: ProjectId, tokens: &[String], limit: usize) -> Result<Vec<PageId>> {
        let match_expr = tokens
            .iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");

        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT p.id FROM pages_fts
             JOIN pages p ON p.rowid = pages_fts.rowid
             WHERE pages_fts MATCH ?1
               AND p.project_id = ?2 AND p.is_latest = 1 AND p.status != 'superseded'
             ORDER BY bm25(pages_fts) ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![match_expr, project_id.to_string(), limit as i64],
            |row| row.get::<_, String>(0),
        )?;
        collect_ids(rows)
    }

    /// Entity stream: pages declaring an entity the query names, ranked by
    /// inverse document frequency — an entity naming three pages carries more
    /// signal than one naming thirty.
    fn entity_stream(&self, project_id: ProjectId, tokens: &[String], limit: usize) -> Result<Vec<PageId>> {
        let placeholders = placeholders(tokens.len());
        let sql = format!(
            "SELECT pe.page_id FROM page_entities pe
             JOIN entities e ON e.id = pe.entity_id
             JOIN pages p ON p.id = pe.page_id
             WHERE e.project_id = ? AND lower(e.name) IN ({placeholders})
               AND p.is_latest = 1 AND p.status != 'superseded'
             GROUP BY pe.page_id
             ORDER BY SUM(
                 1.0 / (SELECT COUNT(*) FROM page_entities pe2 WHERE pe2.entity_id = pe.entity_id)
             ) DESC
             LIMIT ?"
        );

        let mut values: Vec<Value> = vec![Value::Text(project_id.to_string())];
        values.extend(tokens.iter().map(|token| Value::Text(token.clone())));
        values.push(Value::Integer(limit as i64));

        let conn = self.connection();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), |row| row.get::<_, String>(0))?;
        collect_ids(rows)
    }

    /// Link-neighbour stream: pages one hop away, in either direction, from
    /// the pages the other two streams already found relevant.
    fn link_stream(&self, project_id: ProjectId, seeds: &[PageId], limit: usize) -> Result<Vec<PageId>> {
        if seeds.is_empty() {
            return Ok(Vec::new());
        }
        let seed_list = placeholders(seeds.len());
        let sql = format!(
            "SELECT links.page_id FROM (
                 SELECT to_page_id AS page_id FROM page_links
                 WHERE from_page_id IN ({seed_list}) AND to_page_id IS NOT NULL
                 UNION ALL
                 SELECT from_page_id AS page_id FROM page_links
                 WHERE to_page_id IN ({seed_list})
             ) links
             JOIN pages p ON p.id = links.page_id
             WHERE p.project_id = ? AND p.is_latest = 1 AND p.status != 'superseded'
             GROUP BY links.page_id
             ORDER BY COUNT(*) DESC
             LIMIT ?"
        );

        let mut values: Vec<Value> = seeds.iter().map(|id| Value::Text(id.to_string())).collect();
        values.extend(seeds.iter().map(|id| Value::Text(id.to_string())));
        values.push(Value::Text(project_id.to_string()));
        values.push(Value::Integer(limit as i64));

        let conn = self.connection();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), |row| row.get::<_, String>(0))?;
        collect_ids(rows)
    }

    /// Load the fields every stream's candidates are rendered from, in one
    /// query rather than one round trip per id.
    fn load_page_rows(&self, ids: &[PageId]) -> Result<HashMap<PageId, PageRow>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = placeholders(ids.len());
        let sql = format!(
            "SELECT id, path, title, body, tier, status, pinned, canonical
             FROM pages WHERE id IN ({placeholders})"
        );
        let values: Vec<Value> = ids.iter().map(|id| Value::Text(id.to_string())).collect();

        let conn = self.connection();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?;

        let mut out = HashMap::new();
        for row in rows {
            let (id, path, title, body, tier, status, pinned, canonical) = row?;
            out.insert(
                parse_page_id(id),
                PageRow {
                    path: parse_page_path(&path),
                    title,
                    body,
                    tier: parse_tier(&tier),
                    status: parse_page_status(&status),
                    pinned,
                    canonical,
                },
            );
        }
        Ok(out)
    }
}

/// Collect a query's rows into page ids, stopping at the first parse failure.
fn collect_ids(
    rows: impl Iterator<Item = rusqlite::Result<String>>,
) -> Result<Vec<PageId>> {
    rows.map(|row| row.map(parse_page_id))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Split a query into lowercase alphanumeric tokens, deduplicated in order.
///
/// Punctuation is discarded rather than escaped, which is what keeps the FTS5
/// MATCH expression built from these tokens safe: nothing left in a token can
/// be mistaken for query syntax.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        if !tokens.contains(&lower) {
            tokens.push(lower);
        }
    }
    tokens
}

/// A leading slice of a page body, for a hit that has not been opened yet.
fn snippet_of(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= SNIPPET_LEN {
        trimmed.to_owned()
    } else {
        let head: String = trimmed.chars().take(SNIPPET_LEN).collect();
        format!("{head}…")
    }
}

/// `?,?,...` for a dynamic-length `IN` clause.
fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

/// Parse a page identifier written by this crate.
fn parse_page_id(raw: String) -> PageId {
    raw.parse()
        .unwrap_or_else(|error| panic!("stored page id {raw:?} is not a uuid: {error:?}"))
}

/// Parse a page path written by this crate.
fn parse_page_path(raw: &str) -> PagePath {
    PagePath::parse(raw)
        .unwrap_or_else(|error| panic!("stored page path {raw:?} is invalid: {error:?}"))
}

/// Map a stored tier back to its variant.
fn parse_tier(raw: &str) -> Tier {
    match raw {
        "working" => Tier::Working,
        "semantic" => Tier::Semantic,
        "procedural" => Tier::Procedural,
        _ => Tier::Episodic,
    }
}

/// Map a stored page status back to its variant.
fn parse_page_status(raw: &str) -> PageStatus {
    match raw {
        "historical" => PageStatus::Historical,
        "do-not-answer-from" => PageStatus::DoNotAnswerFrom,
        "superseded" => PageStatus::Superseded,
        _ => PageStatus::Active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::WorkspaceId;
    use anamnesis_core::page::{Frontmatter, Page, PagePath as CorePagePath, Tier as CoreTier};
    use anamnesis_core::scope::resolve_scope;

    fn fixture() -> (tempfile::TempDir, Store, ProjectId, WorkspaceId) {
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

        (dir, store, scope.project_id, scope.workspace_id)
    }

    fn now() -> Timestamp {
        "2026-08-24T09:00:00Z".parse().expect("timestamp")
    }

    fn write_page(
        store: &Store,
        project_id: ProjectId,
        path: &str,
        title: &str,
        body: &str,
        entities: Vec<Entity>,
    ) -> PageId {
        let path = CorePagePath::parse(path).expect("path");
        let mut frontmatter = Frontmatter::new(title, entities.clone()).unwrap();
        frontmatter.tier = CoreTier::Semantic;
        let page = Page::new(project_id, path, frontmatter, body);
        store.upsert_page(&page, now()).expect("upsert");
        store
            .set_page_entities(project_id, page.id, &entities)
            .expect("entities");
        page.id
    }

    #[test]
    fn full_text_finds_a_page_by_body_content() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "decisions/0001-storage.md",
            "Storage engine",
            "We chose SQLite because the index is rebuildable from the wiki.",
            Vec::new(),
        );

        let hits = store.query_pages(project, "sqlite rebuildable", 10, now()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Storage engine");
    }

    #[test]
    fn a_pinned_authoritative_page_outranks_a_plain_one_on_a_shared_term() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "notes/aside.md",
            "Aside",
            "SQLite came up in passing here.",
            Vec::new(),
        );
        let decision = CorePagePath::parse("decisions/0002-sqlite.md").unwrap();
        let mut frontmatter = Frontmatter::new("Why SQLite", Vec::new()).unwrap();
        frontmatter.pinned = true;
        frontmatter.canonical = true;
        let page = Page::new(project, decision, frontmatter, "SQLite is the storage engine.");
        store.upsert_page(&page, now()).unwrap();

        let hits = store.query_pages(project, "sqlite", 10, now()).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Why SQLite", "authority should win the tie");
    }

    #[test]
    fn entity_matching_finds_a_page_full_text_would_miss() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "concepts/react.md",
            "Frontend framework",
            "The team picked a component model for the dashboard.",
            vec![Entity::parse("React").unwrap()],
        );

        let hits = store.query_pages(project, "React", 10, now()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Frontend framework");
    }

    #[test]
    fn link_neighbours_are_pulled_in_around_a_direct_hit() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "decisions/0001-storage.md",
            "Storage engine",
            "We chose SQLite for the index.",
            Vec::new(),
        );
        let neighbor = write_page(
            &store,
            project,
            "gotchas/windows-bom.md",
            "Windows BOM",
            "This is a separate concern, but see the storage decision.",
            Vec::new(),
        );
        store
            .set_page_links(project, neighbor, &["decisions/0001-storage.md".to_owned()])
            .unwrap();

        let hits = store.query_pages(project, "sqlite", 10, now()).unwrap();
        assert!(hits.iter().any(|hit| hit.title == "Windows BOM"));
    }

    #[test]
    fn superseded_pages_are_excluded_from_every_stream() {
        let (_dir, store, project, _workspace) = fixture();
        let path = CorePagePath::parse("decisions/0001-storage.md").unwrap();
        let mut frontmatter = Frontmatter::new("Old storage decision", Vec::new()).unwrap();
        frontmatter.status = anamnesis_core::page::PageStatus::Superseded;
        let page = Page::new(project, path, frontmatter, "SQLite, superseded now.");
        store.upsert_page(&page, now()).unwrap();

        let hits = store.query_pages(project, "sqlite", 10, now()).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn an_empty_query_returns_nothing_rather_than_everything() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(&store, project, "notes/a.md", "A", "body", Vec::new());
        assert!(store.query_pages(project, "   ", 10, now()).unwrap().is_empty());
    }

    #[test]
    fn a_returned_hit_has_its_access_recorded() {
        let (_dir, store, project, _workspace) = fixture();
        let id = write_page(&store, project, "notes/a.md", "A", "sqlite notes", Vec::new());

        store.query_pages(project, "sqlite", 10, now()).unwrap();

        let conn = store.connection();
        let access_count: i64 = conn
            .query_row(
                "SELECT access_count FROM pages WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(access_count, 1);
    }
}
