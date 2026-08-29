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
use anamnesis_core::retrieval::{RRF_K, Tuning, fuse_and_rank, fuse_weighted, tokenize};
use jiff::Timestamp;
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::convert::{parse_id, parse_page_path};
use crate::{Result, Store};

/// Longest snippet returned with a hit, in characters.
const SNIPPET_LEN: usize = 240;

/// A page returned by [`Store::query_pages`], ranked by fused relevance.
#[derive(Debug, Clone, PartialEq)]
pub struct PageHit {
    /// Identifies the page.
    pub page_id: PageId,
    /// The project the page belongs to.
    ///
    /// Worth carrying because a ranking can now span scopes: a policy from the
    /// workspace's shared scope and a note about this project are different
    /// kinds of answer, and the path alone does not say which is which.
    pub project_id: ProjectId,
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

/// What each retrieval stream found, before fusion picked winners.
///
/// The fused ranking is the answer; this is the working. It exists because
/// every weight in [`Store::query_pages`] was chosen by argument — whether the
/// entity stream earns its place, whether link neighbours help or just add
/// noise — and none of those questions can be answered from a fused list,
/// where a page found by three streams and a page found by one look the same.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamBreakdown {
    /// Full-text matches, best first.
    pub fts: Vec<PageId>,
    /// Pages whose declared entities the query names.
    pub entity: Vec<PageId>,
    /// Neighbours of what the first two streams found.
    pub links: Vec<PageId>,
    /// Cosine neighbours of the query vector. Empty without an embedding.
    pub vectors: Vec<PageId>,
}

impl StreamBreakdown {
    /// Each stream with the name it goes by, in the order they are fused.
    pub fn named(&self) -> [(&'static str, &[PageId]); 4] {
        [
            ("fts", &self.fts),
            ("entity", &self.entity),
            ("links", &self.links),
            ("vectors", &self.vectors),
        ]
    }
}

/// Row shape shared by every stream before fusion picks winners.
struct PageRow {
    project_id: ProjectId,
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
        self.query_pages_with(project_id, query, limit, now, embedding, &Tuning::default())
    }

    /// [`Store::query_pages`], with the fusion constants named rather than
    /// assumed.
    ///
    /// The same call the server makes, which is the point: `anamnesis eval
    /// --sweep` scores settings by running real queries through this, so what
    /// a sweep reports is the ranking that would ship rather than a model of
    /// it. Production passes [`Tuning::default`] and always has.
    pub fn query_pages_with(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        now: Timestamp,
        embedding: Option<(&str, &[f32])>,
        tuning: &Tuning,
    ) -> Result<Vec<PageHit>> {
        let tokens = tokenize(query);
        if tokens.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let depth = tuning.candidates;
        let fts = self.fts_stream(project_id, &tokens, depth)?;
        let entity = self.entity_stream(project_id, &tokens, depth)?;

        let mut seeds: Vec<PageId> = Vec::with_capacity(fts.len() + entity.len());
        for id in fts.iter().chain(entity.iter()) {
            if !seeds.contains(id) {
                seeds.push(*id);
            }
        }
        seeds.truncate(depth);
        let links = self.link_stream(project_id, &seeds, depth, tuning.rrf_k)?;

        let vectors = match embedding {
            Some((model, vector)) if !vector.is_empty() => {
                self.vector_stream(project_id, model, vector, depth)?
            }
            _ => Vec::new(),
        };

        let weights = tuning.weights();
        let fused = fuse_weighted(
            &[
                (fts.as_slice(), weights[0]),
                (entity.as_slice(), weights[1]),
                (links.as_slice(), weights[2]),
                (vectors.as_slice(), weights[3]),
            ],
            tuning.rrf_k,
        );
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
                    * tuning.authority(row.pinned, row.canonical, row.path.is_authoritative());
                Some(PageHit {
                    page_id: id,
                    project_id: row.project_id,
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

    /// Run each stream and return them unfused.
    ///
    /// The diagnostic counterpart to [`Store::query_pages`], which this
    /// deliberately does not call: fusing is what hides the thing being asked
    /// about.
    ///
    /// **Nothing here records an access.** `query_pages` bumps the statistics
    /// for everything it returns, because a page it returned was a page
    /// somebody was given. Asking which stream *would have* found a page is
    /// not that, and the decay sweep reads those counters to decide what to
    /// keep — an explain that inflated them would make the pages it was run
    /// over harder to forget.
    pub fn query_streams(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        embedding: Option<(&str, &[f32])>,
        tuning: &Tuning,
    ) -> Result<StreamBreakdown> {
        let tokens = tokenize(query);
        if tokens.is_empty() || limit == 0 {
            return Ok(StreamBreakdown::default());
        }

        let fts = self.fts_stream(project_id, &tokens, limit)?;
        let entity = self.entity_stream(project_id, &tokens, limit)?;

        // Seeded exactly as `query_pages` seeds it, or the link stream here
        // would be answering a different question from the one that runs.
        let mut seeds: Vec<PageId> = Vec::with_capacity(fts.len() + entity.len());
        for id in fts.iter().chain(entity.iter()) {
            if !seeds.contains(id) {
                seeds.push(*id);
            }
        }
        seeds.truncate(tuning.candidates);
        let links = self.link_stream(project_id, &seeds, limit, tuning.rrf_k)?;

        let vectors = match embedding {
            Some((model, vector)) if !vector.is_empty() => {
                self.vector_stream(project_id, model, vector, limit)?
            }
            _ => Vec::new(),
        };

        Ok(StreamBreakdown {
            fts,
            entity,
            links,
            vectors,
        })
    }

    /// Search one project and the scopes it inherits from, as one ranking.
    ///
    /// `inherited` is the shared scopes — today that means the workspace's
    /// `_global`, which holds what applies to every project in it. A page
    /// there is a policy somebody wrote once and expects to see everywhere,
    /// and until this existed it was a file nobody read.
    ///
    /// Each scope is searched on its own and the rankings are fused, rather
    /// than the streams being widened to select across projects. Two reasons.
    /// The rankings are what fusion is defined over — RRF combines by rank
    /// precisely because scores from different sources are not comparable, and
    /// two projects are two sources. And a page's authority multiplier is
    /// relative to the corpus it sits in: a canonical page in a five-page
    /// global scope should not outrank one in a five-hundred-page project
    /// merely for having less competition.
    ///
    /// Ties go to the project. A global page and a local page that fuse to the
    /// same score are not equally good answers — the one written about *this*
    /// project is the more specific, and specificity is the whole reason the
    /// two scopes are separate.
    pub fn query_pages_across(
        &self,
        project_id: ProjectId,
        inherited: &[ProjectId],
        query: &str,
        limit: usize,
        now: Timestamp,
        embedding: Option<(&str, &[f32])>,
    ) -> Result<Vec<PageHit>> {
        let own = self.query_pages(project_id, query, limit, now, embedding)?;
        if inherited.is_empty() {
            return Ok(own);
        }

        // Every scope contributes at most `limit`, so a shared scope cannot
        // crowd out the project by being large.
        let mut rankings: Vec<Vec<PageId>> = vec![own.iter().map(|hit| hit.page_id).collect()];
        let mut hits: HashMap<PageId, PageHit> =
            own.into_iter().map(|hit| (hit.page_id, hit)).collect();
        let local: Vec<PageId> = rankings[0].clone();

        for scope in inherited {
            if *scope == project_id {
                continue;
            }
            let found = self.query_pages(*scope, query, limit, now, embedding)?;
            rankings.push(found.iter().map(|hit| hit.page_id).collect());
            for hit in found {
                hits.entry(hit.page_id).or_insert(hit);
            }
        }

        let fused = fuse_and_rank(&rankings, RRF_K);
        let mut merged: Vec<PageHit> = fused
            .into_iter()
            .filter_map(|(id, score)| {
                let mut hit = hits.remove(&id)?;
                hit.score = score;
                Some(hit)
            })
            .collect();

        merged.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| local.contains(&b.page_id).cmp(&local.contains(&a.page_id)))
        });
        merged.truncate(limit);
        Ok(merged)
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
            scored.push((parse_id(id), cosine_similarity(query_vector, &vector)));
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

            // Split once, here, so the match can compare like with like. Done
            // on every write rather than only on insert: it costs one
            // statement per token and it is what backfills the names that
            // were stored before this table existed.
            for token in tokenize(entity.as_str()) {
                tx.execute(
                    "INSERT INTO entity_tokens (entity_id, token) VALUES (?1, ?2)
                     ON CONFLICT (entity_id, token) DO NOTHING",
                    params![entity_id, token],
                )?;
            }
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
                    // Not filtered by `is_latest`: a link naming a page that
                    // has since been superseded still names a page that
                    // exists. Whether it is the head of its chain decides how
                    // it ranks, not whether it is there — and treating it as
                    // missing would have the wiki asking for a page it
                    // already holds.
                    "SELECT id FROM pages
                     WHERE project_id = ?1
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
        // Two ways to match, and the first is the one that matters: an entity
        // matches when *every* token of its name is in the query, so
        // `Windows BOM` is found by "windows bom" and not by "windows" alone.
        // Requiring all of them keeps a two-word name from answering half a
        // question; the other three streams are what find a page from half a
        // name.
        //
        // The second is for entities stored before their tokens were: those
        // rows have no `entity_tokens` at all, and until something rewrites
        // the page — or `anamnesis reindex` does — they can still be matched
        // whole, exactly as they were.
        let sql = format!(
            "SELECT pe.page_id FROM page_entities pe
             JOIN entities e ON e.id = pe.entity_id
             JOIN pages p ON p.id = pe.page_id
             WHERE e.project_id = ?
               AND (
                 e.id IN (
                   SELECT et.entity_id FROM entity_tokens et
                   JOIN entities scoped ON scoped.id = et.entity_id
                   WHERE scoped.project_id = ?
                   GROUP BY et.entity_id
                   HAVING COUNT(*) > 0
                      AND COUNT(*) = SUM(CASE WHEN et.token IN ({placeholders}) THEN 1 ELSE 0 END)
                 )
                 OR lower(e.name) IN ({placeholders})
               )
               AND p.is_latest = 1 AND p.status != 'superseded'
             GROUP BY pe.page_id
             ORDER BY SUM(
                 1.0 / (SELECT COUNT(*) FROM page_entities pe2 WHERE pe2.entity_id = pe.entity_id)
             ) DESC
             LIMIT ?"
        );

        let mut values: Vec<Value> = vec![
            Value::Text(project_id.to_string()),
            Value::Text(project_id.to_string()),
        ];
        values.extend(tokens.iter().map(|token| Value::Text(token.clone())));
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
    ///
    /// Each edge counts for what its seed was worth, `1 / (k + rank)` — the
    /// same reciprocal-rank form fusion uses, and the same `k`, because both
    /// are answering the question of how much a ranking's order should matter.
    ///
    /// It used to count edges, `ORDER BY COUNT(*)`, which threw the seed's rank
    /// away: a neighbour of the best full-text hit and a neighbour of the
    /// thirtieth ranked identically, and two neighbours of the thirtieth beat
    /// one neighbour of the first. That is backwards. A neighbour is not
    /// evidence about itself — it is evidence about the page that pointed at
    /// it, and worth exactly what that page was worth.
    fn link_stream(
        &self,
        project_id: ProjectId,
        seeds: &[PageId],
        limit: usize,
        k: f64,
    ) -> Result<Vec<PageId>> {
        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        let seed_rows = vec!["(?, ?)"; seeds.len()].join(", ");
        let sql = format!(
            "WITH seeds(id, weight) AS (VALUES {seed_rows})
             SELECT links.page_id FROM (
                 SELECT from_page_id AS seed_id, to_page_id AS page_id FROM page_links
                 WHERE from_page_id IN (SELECT id FROM seeds) AND to_page_id IS NOT NULL
                 UNION ALL
                 SELECT to_page_id AS seed_id, from_page_id AS page_id FROM page_links
                 WHERE to_page_id IN (SELECT id FROM seeds)
             ) links
             JOIN pages p ON p.id = links.page_id
             JOIN seeds s ON s.id = links.seed_id
             WHERE p.project_id = ? AND p.is_latest = 1 AND p.status != 'superseded'
             GROUP BY links.page_id
             ORDER BY SUM(s.weight) DESC, links.page_id ASC
             LIMIT ?"
        );

        let mut values: Vec<Value> = Vec::with_capacity(seeds.len() * 2 + 2);
        for (rank, id) in seeds.iter().enumerate() {
            values.push(Value::Text(id.to_string()));
            values.push(Value::Real(1.0 / (k + rank as f64 + 1.0)));
        }
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
            "SELECT id, project_id, path, title, body, tier, status, pinned, canonical
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
                row.get::<_, String>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, bool>(8)?,
            ))
        })?;

        let mut out = HashMap::new();
        for row in rows {
            let (id, project_id, path, title, body, tier, status, pinned, canonical) = row?;
            out.insert(
                parse_id(id),
                PageRow {
                    project_id: parse_id(project_id),
                    path: parse_page_path(&path),
                    title,
                    body,
                    tier: Tier::from_storage(&tier),
                    status: PageStatus::from_storage(&status),
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
    rows.map(|row| row.map(parse_id))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
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
pub(crate) fn placeholders(count: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::fixture;
    use anamnesis_core::page::{Frontmatter, Page, PagePath as CorePagePath, Tier as CoreTier};

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
    fn a_two_word_entity_is_found_by_the_query_someone_would_type() {
        // Stored whole, this name could never equal a token, because a query
        // is split before it is matched. The page was reachable through full
        // text and through nothing else.
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "gotchas/encoding.md",
            "Encoding trap",
            "PowerShell prepends three bytes when piping to a native exe.",
            vec![Entity::parse("Windows BOM").unwrap()],
        );

        let hits = store
            .query_pages(project, "windows bom", 10, now(), None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Encoding trap");
    }

    #[test]
    fn a_hyphenated_entity_is_found_however_the_separator_is_typed() {
        let (_dir, store, project, _workspace) = fixture();
        // Nothing in the path, title, or body carries either token, so a hit
        // can only have come through the entity.
        write_page(
            &store,
            project,
            "notes/provider.md",
            "The provider crate",
            "Where the provider trait lives.",
            vec![Entity::parse("anamnesis-llm").unwrap()],
        );

        for query in ["anamnesis-llm", "anamnesis llm", "ANAMNESIS-LLM"] {
            let hits = store.query_pages(project, query, 10, now(), None).unwrap();
            assert_eq!(hits.len(), 1, "no hit for {query:?}");
        }
    }

    #[test]
    fn half_a_name_is_not_a_match_for_the_whole_name() {
        // All of a name's tokens have to be present. A two-word entity that
        // answered to either word would drown the stream it shares with three
        // others; half a name is what full text is for.
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "gotchas/encoding.md",
            "Encoding trap",
            "PowerShell prepends three bytes when piping to a native exe.",
            vec![Entity::parse("Windows BOM").unwrap()],
        );

        assert!(
            store
                .query_pages(project, "windows", 10, now(), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_query_naming_more_than_the_entity_still_matches_it() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "gotchas/encoding.md",
            "Encoding trap",
            "PowerShell prepends three bytes when piping to a native exe.",
            vec![Entity::parse("Windows BOM").unwrap()],
        );

        let hits = store
            .query_pages(
                project,
                "why does the windows bom break docker",
                10,
                now(),
                None,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_entity_stored_before_its_tokens_were_still_matches() {
        // What every row in an existing database looks like until something
        // rewrites its page. Matching them whole, as before, is what keeps
        // this from being a retrieval regression that only a rebuild fixes.
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "concepts/react.md",
            "Frontend framework",
            "The team picked a component model for the dashboard.",
            vec![Entity::parse("React").unwrap()],
        );
        store
            .connection()
            .execute("DELETE FROM entity_tokens", [])
            .expect("drop the tokens");

        let hits = store
            .query_pages(project, "React", 10, now(), None)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn rewriting_a_page_backfills_the_tokens_of_an_older_entity() {
        let (_dir, store, project, _workspace) = fixture();
        let entities = vec![Entity::parse("Windows BOM").unwrap()];
        write_page(
            &store,
            project,
            "gotchas/encoding.md",
            "Encoding trap",
            "PowerShell prepends three bytes when piping to a native exe.",
            entities.clone(),
        );
        store
            .connection()
            .execute("DELETE FROM entity_tokens", [])
            .expect("drop the tokens");
        assert!(
            store
                .query_pages(project, "windows bom", 10, now(), None)
                .unwrap()
                .is_empty(),
            "an untokenized two-word name is unreachable, which is the bug"
        );

        write_page(
            &store,
            project,
            "gotchas/encoding.md",
            "Encoding trap",
            "PowerShell prepends three bytes when piping to a native exe.",
            entities,
        );

        assert_eq!(
            store
                .query_pages(project, "windows bom", 10, now(), None)
                .unwrap()
                .len(),
            1,
            "writing the page again splits the name it already had"
        );
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

    /// A neighbour is worth what the page that pointed at it was worth.
    ///
    /// The stream used to rank neighbours by `COUNT(*)` of the edges reaching
    /// them, which threw the seed's own rank away: a neighbour of the best
    /// full-text hit and a neighbour of the thirtieth ranked identically.
    ///
    /// Both queries here reach both seeds, so the seed *set* is the same and
    /// only its order differs — which is exactly what counting edges cannot
    /// see. The neighbour of whichever page the query is really about has to
    /// come first.
    #[test]
    fn a_neighbour_of_a_better_seed_comes_first() {
        let (_dir, store, project, _workspace) = fixture();

        let alpha = write_page(
            &store,
            project,
            "notes/alpha.md",
            "Alpha",
            "widget alpha alpha alpha",
            Vec::new(),
        );
        let beta = write_page(
            &store,
            project,
            "notes/beta.md",
            "Beta",
            "widget beta beta beta",
            Vec::new(),
        );
        let near_alpha = write_page(
            &store,
            project,
            "notes/near-alpha.md",
            "Near alpha",
            "Nothing either query says.",
            Vec::new(),
        );
        let near_beta = write_page(
            &store,
            project,
            "notes/near-beta.md",
            "Near beta",
            "Nothing either query says.",
            Vec::new(),
        );

        store
            .set_page_links(project, alpha, &["notes/near-alpha.md".to_owned()])
            .unwrap();
        store
            .set_page_links(project, beta, &["notes/near-beta.md".to_owned()])
            .unwrap();

        let links_for = |query: &str| -> Vec<PageId> {
            store
                .query_streams(project, query, 10, None, &Tuning::default())
                .expect("streams")
                .links
        };

        let about_alpha = links_for("widget alpha");
        let about_beta = links_for("widget beta");

        assert_eq!(
            about_alpha.first(),
            Some(&near_alpha),
            "the neighbour of the page the query is about should lead"
        );
        assert_eq!(
            about_beta.first(),
            Some(&near_beta),
            "and it should change when the query changes which page that is"
        );
        assert!(
            about_alpha.contains(&near_beta) && about_beta.contains(&near_alpha),
            "both neighbours stay in the stream; what changes is their order"
        );
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

    /// The scope's whole purpose: a policy written once, found from a project
    /// that has never mentioned it. Before this, `_global/` was a directory
    /// nobody read.
    #[test]
    fn a_project_query_reaches_the_shared_scope() {
        let (dir, store, project, _workspace) = fixture();
        let global = global_scope(&dir);
        store.upsert_project(&global, now()).expect("global");

        write_page(
            &store,
            project,
            "notes/local.md",
            "Local note",
            "Nothing about databases here.",
            Vec::new(),
        );
        write_page(
            &store,
            global.project_id,
            "policy/databases.md",
            "We use PostgreSQL",
            "Every project in this workspace stores its data in PostgreSQL.",
            Vec::new(),
        );

        let own = store
            .query_pages(project, "postgresql", 5, now(), None)
            .expect("query");
        assert!(own.is_empty(), "the project alone cannot know this");

        let shared = store
            .query_pages_across(project, &[global.project_id], "postgresql", 5, now(), None)
            .expect("query");
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].path.as_str(), "policy/databases.md");
    }

    /// Specificity is the reason the two scopes are separate at all, so when
    /// they agree the project's own page is the one to read first.
    #[test]
    fn the_projects_own_page_wins_a_tie() {
        let (dir, store, project, _workspace) = fixture();
        let global = global_scope(&dir);
        store.upsert_project(&global, now()).expect("global");

        // The same words in both scopes: each ranks first where it lives, so
        // the fused scores are equal and only the tie-break separates them.
        write_page(
            &store,
            project,
            "notes/style.md",
            "Style",
            "Migrations are numbered and never edited.",
            Vec::new(),
        );
        write_page(
            &store,
            global.project_id,
            "policy/style.md",
            "Style",
            "Migrations are numbered and never edited.",
            Vec::new(),
        );

        let hits = store
            .query_pages_across(project, &[global.project_id], "migrations", 5, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].path.as_str(),
            "notes/style.md",
            "the global page displaced the project's own"
        );
    }

    /// Inheriting nothing has to mean exactly what a plain query means, or
    /// every existing caller changes behaviour by being ported.
    #[test]
    fn inheriting_nothing_answers_as_a_plain_query_does() {
        let (_dir, store, project, _workspace) = fixture();
        write_page(
            &store,
            project,
            "notes/a.md",
            "A",
            "sqlite and rusqlite",
            Vec::new(),
        );

        let plain = store
            .query_pages(project, "sqlite", 5, now(), None)
            .expect("query");
        let across = store
            .query_pages_across(project, &[], "sqlite", 5, now(), None)
            .expect("query");

        assert_eq!(
            plain.iter().map(|hit| hit.page_id).collect::<Vec<_>>(),
            across.iter().map(|hit| hit.page_id).collect::<Vec<_>>()
        );
    }

    /// A shared scope is read, not merged: the pages stay in their own project
    /// and are still reachable there on their own.
    #[test]
    fn a_shared_page_still_belongs_to_the_shared_scope() {
        let (dir, store, project, _workspace) = fixture();
        let global = global_scope(&dir);
        store.upsert_project(&global, now()).expect("global");
        write_page(
            &store,
            global.project_id,
            "policy/databases.md",
            "We use PostgreSQL",
            "Every project stores its data in PostgreSQL.",
            Vec::new(),
        );

        assert_eq!(store.page_count(project).expect("count"), 0);
        assert_eq!(store.page_count(global.project_id).expect("count"), 1);
    }

    /// The workspace's shared scope, rooted where its pages would live.
    fn global_scope(dir: &tempfile::TempDir) -> anamnesis_core::scope::ResolvedScope {
        anamnesis_core::scope::ResolvedScope::global(
            &anamnesis_core::scope::WorkspaceName::default(),
            dir.path().join("_global"),
        )
    }
}
