//! Retrieval: fusing full-text search, entity matching, and link-neighbour
//! expansion into one ranked list of pages.
//!
//! Four independent queries each produce a ranked stream of page ids;
//! turning several rankings into one is [`anamnesis_core::retrieval`], which
//! is pure and knows nothing about SQL. Everything here is the SQL (and, for
//! the vector stream, the arithmetic) that produces the streams, plus the
//! bookkeeping (authority multiplier, access recording) that surrounds fusing
//! them.
//!
//! The vector stream is opt-in: it only runs when a caller supplies a query
//! embedding, because computing one means loading a local model this crate
//! knows nothing about (see `anamnesis_llm::embed`). Without one, relevance
//! comes from the other three signals, same as before this stream existed.

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
    ///
    /// `embedding`, when supplied, is `(model, query vector)`: the fourth
    /// stream, cosine similarity against every page embedded under that model
    /// name. A page embedded under a different model (or not embedded at all)
    /// simply does not appear in that stream — it is still reachable through
    /// the other three.
    pub fn query_pages(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        now: Timestamp,
        embedding: Option<(&str, &[f32])>,
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

        let vectors = match embedding {
            Some((model, vector)) if !vector.is_empty() => {
                self.vector_stream(project_id, model, vector, STREAM_CANDIDATES)?
            }
            _ => Vec::new(),
        };

        let fused = fuse_and_rank(&[fts, entity, links, vectors], RRF_K);
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

    /// Store (or replace) a page's embedding under one model.
    ///
    /// Keyed by `(page_id, model)` rather than `page_id` alone, so re-running
    /// this under a new model name adds a second row instead of overwriting a
    /// vector that other, not-yet-migrated queries might still compare against.
    pub fn set_page_embedding(&self, page_id: PageId, model: &str, vector: &[f32]) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO page_embeddings (page_id, model, dim, vector) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (page_id, model) DO UPDATE SET dim = excluded.dim, vector = excluded.vector",
            params![page_id.to_string(), model, vector.len() as i64, vector_to_bytes(vector)],
        )?;
        Ok(())
    }

    /// Vector stream: pages embedded under `model`, ranked by cosine
    /// similarity to `query_vector`.
    ///
    /// Brute force — every embedded page in the project is scored on every
    /// call. Fine at the scale this system targets (a project's wiki, not a
    /// search engine's corpus); an ANN index is a later problem, not a
    /// day-one dependency.
    fn vector_stream(
        &self,
        project_id: ProjectId,
        model: &str,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<PageId>> {
        let conn = self.connection();
        let mut statement = conn.prepare(
            "SELECT pe.page_id, pe.vector FROM page_embeddings pe
             JOIN pages p ON p.id = pe.page_id
             WHERE pe.model = ?1 AND p.project_id = ?2
               AND p.is_latest = 1 AND p.status != 'superseded'",
        )?;
        let rows = statement.query_map(params![model, project_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;

        let mut scored: Vec<(PageId, f32)> = Vec::new();
        for row in rows {
            let (id, bytes) = row?;
            let vector = bytes_to_vector(&bytes);
            scored.push((parse_page_id(id), cosine_similarity(query_vector, &vector)));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(id, _)| id).collect())
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

    /// Replace a page's outgoing wikilinks, and resolve any link that was
    /// waiting for this page to exist.
    ///
    /// An unresolved target is not an error: `[[some-page]]` pointing at a
    /// page that does not exist yet is intent, not a mistake. Both directions
    /// have to be handled here, and forgetting the second is a silent bug —
    /// the link is recorded, nothing errors, and the link-neighbour retrieval
    /// stream simply never sees that edge:
    ///
    /// * **outgoing** — every target this page names is resolved against the
    ///   pages that exist now;
    /// * **incoming** — every *other* page's unresolved link naming this
    ///   page's path is resolved to it, which is what makes writing pages in
    ///   any order safe.
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

        // The `to_target || '.md'` half matches the extension-less form
        // `[[gotchas/windows-bom]]`, the same two spellings the outgoing
        // lookup above accepts.
        let path: Option<String> = tx
            .query_row(
                "SELECT path FROM pages WHERE id = ?1",
                params![page_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(path) = path {
            tx.execute(
                "UPDATE page_links SET to_page_id = ?1
                 WHERE to_page_id IS NULL
                   AND (to_target = ?2 OR to_target || '.md' = ?2)
                   AND from_page_id IN (SELECT id FROM pages WHERE project_id = ?3)",
                params![page_id.to_string(), path, project_id.to_string()],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Full-text stream: pages whose indexed content matches any query token,
    /// ranked by SQLite's bm25 (lower is a better match).
    fn fts_stream(
        &self,
        project_id: ProjectId,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<PageId>> {
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
    fn entity_stream(
        &self,
        project_id: ProjectId,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<PageId>> {
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
        let rows = statement.query_map(params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        collect_ids(rows)
    }

    /// Link-neighbour stream: pages one hop away, in either direction, from
    /// the pages the other two streams already found relevant.
    fn link_stream(
        &self,
        project_id: ProjectId,
        seeds: &[PageId],
        limit: usize,
    ) -> Result<Vec<PageId>> {
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
        let rows = statement.query_map(params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?;
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
fn collect_ids(rows: impl Iterator<Item = rusqlite::Result<String>>) -> Result<Vec<PageId>> {
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

/// Serialize a vector as little-endian `f32` bytes.
fn vector_to_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Deserialize a vector written by [`vector_to_bytes`].
///
/// A byte count that is not a multiple of four means the row was not written
/// by this crate; the trailing partial value is dropped rather than causing a
/// panic, since a search stream is not worth failing a whole query over.
fn bytes_to_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Cosine similarity of two vectors. `0.0` for a length mismatch or either
/// vector being zero, rather than a divide-by-zero `NaN` that would poison
/// every sort it touches.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
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

        let hits = store
            .query_pages(project, "sqlite rebuildable", 10, now(), None)
            .unwrap();
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
        let page = Page::new(
            project,
            decision,
            frontmatter,
            "SQLite is the storage engine.",
        );
        store.upsert_page(&page, now()).unwrap();

        let hits = store
            .query_pages(project, "sqlite", 10, now(), None)
            .unwrap();
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

        let hits = store
            .query_pages(project, "React", 10, now(), None)
            .unwrap();
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

        let hits = store
            .query_pages(project, "sqlite", 10, now(), None)
            .unwrap();
        assert!(hits.iter().any(|hit| hit.title == "Windows BOM"));
    }

    #[test]
    fn a_link_written_before_its_target_resolves_when_the_target_arrives() {
        let (_dir, store, project, _workspace) = fixture();

        // Written first, naming a page that does not exist yet — the ordinary
        // case when someone writes a decision that points at a gotcha they
        // have not written down yet.
        let source = write_page(
            &store,
            project,
            "decisions/0001-storage.md",
            "Storage engine",
            "We chose SQLite. See [[gotchas/windows-bom.md]].",
            Vec::new(),
        );
        store
            .set_page_links(project, source, &["gotchas/windows-bom.md".to_owned()])
            .unwrap();

        let target = write_page(
            &store,
            project,
            "gotchas/windows-bom.md",
            "Windows BOM",
            "PowerShell prepends a BOM when piping to a native exe.",
            Vec::new(),
        );
        store.set_page_links(project, target, &[]).unwrap();

        let resolved: Option<String> = store
            .connection()
            .query_row(
                "SELECT to_page_id FROM page_links WHERE from_page_id = ?1",
                params![source.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            resolved.as_deref(),
            Some(target.to_string().as_str()),
            "the backlink should have been resolved when its target was written"
        );

        // And the edge is live: searching for the source's content now pulls
        // the target in through the link-neighbour stream.
        let hits = store
            .query_pages(project, "sqlite", 10, now(), None)
            .unwrap();
        assert!(hits.iter().any(|hit| hit.title == "Windows BOM"));
    }

    #[test]
    fn an_extensionless_link_resolves_to_the_markdown_page() {
        let (_dir, store, project, _workspace) = fixture();
        let source = write_page(
            &store,
            project,
            "notes/a.md",
            "A",
            "See [[notes/b]].",
            Vec::new(),
        );
        store
            .set_page_links(project, source, &["notes/b".to_owned()])
            .unwrap();

        let target = write_page(&store, project, "notes/b.md", "B", "body", Vec::new());
        store.set_page_links(project, target, &[]).unwrap();

        let resolved: Option<String> = store
            .connection()
            .query_row(
                "SELECT to_page_id FROM page_links WHERE from_page_id = ?1",
                params![source.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(resolved.as_deref(), Some(target.to_string().as_str()));
    }

    #[test]
    fn superseded_pages_are_excluded_from_every_stream() {
        let (_dir, store, project, _workspace) = fixture();
        let path = CorePagePath::parse("decisions/0001-storage.md").unwrap();
        let mut frontmatter = Frontmatter::new("Old storage decision", Vec::new()).unwrap();
        frontmatter.status = anamnesis_core::page::PageStatus::Superseded;
        let page = Page::new(project, path, frontmatter, "SQLite, superseded now.");
        store.upsert_page(&page, now()).unwrap();

        let hits = store
            .query_pages(project, "sqlite", 10, now(), None)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn an_empty_query_returns_nothing_rather_than_everything() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(&store, project, "notes/a.md", "A", "body", Vec::new());
        assert!(
            store
                .query_pages(project, "   ", 10, now(), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_returned_hit_has_its_access_recorded() {
        let (_dir, store, project, _workspace) = fixture();
        let id = write_page(
            &store,
            project,
            "notes/a.md",
            "A",
            "sqlite notes",
            Vec::new(),
        );

        store
            .query_pages(project, "sqlite", 10, now(), None)
            .unwrap();

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

    #[test]
    fn a_vector_hit_is_found_even_without_a_shared_token() {
        let (_dir, store, project, _workspace) = fixture();
        // Nothing in this body shares a token with the query, and it names no
        // entity — full-text and entity matching would both miss it. Only the
        // vector stream can surface it.
        let id = write_page(
            &store,
            project,
            "notes/car.md",
            "Automobile",
            "A vehicle with four wheels used for transportation.",
            Vec::new(),
        );
        store
            .set_page_embedding(id, "test-model", &[1.0, 0.0, 0.0])
            .unwrap();

        let hits = store
            .query_pages(
                project,
                "unrelated",
                10,
                now(),
                Some(("test-model", &[1.0, 0.0, 0.0])),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Automobile");
    }

    #[test]
    fn a_vector_under_a_different_model_is_not_matched() {
        let (_dir, store, project, _workspace) = fixture();
        let id = write_page(
            &store,
            project,
            "notes/car.md",
            "Automobile",
            "body",
            Vec::new(),
        );
        store
            .set_page_embedding(id, "model-a", &[1.0, 0.0])
            .unwrap();

        let hits = store
            .query_pages(
                project,
                "nomatch",
                10,
                now(),
                Some(("model-b", &[1.0, 0.0])),
            )
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn cosine_similarity_ignores_magnitude() {
        assert!((cosine_similarity(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn a_vector_round_trips_through_storage_bytes() {
        let original = vec![1.5_f32, -2.25, 0.0, 3.125];
        assert_eq!(bytes_to_vector(&vector_to_bytes(&original)), original);
    }
}
