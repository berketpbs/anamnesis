//! MCP server implementation for anamnesis.
//!
//! Exposes seven tools over the Model Context Protocol: `memory_query`,
//! `memory_read_page`, `memory_write_page`, `memory_patch_page`, `memory_handoff_accept`,
//! `workstream_start`, and `workstream_status`. All of them operate against
//! one resolved scope — the project the server was started against — the same
//! way `anamnesis serve` binds to one project's store and wiki rather than
//! discovering scope per request.
//!
//! Reading is deliberately two tools rather than one. `memory_query` ranks and
//! returns snippets, which is what deciding *which* page needs; reading the
//! page it picked is `memory_read_page`, which returns the body whole. Folding
//! the second into the first would mean either truncating every hit or
//! returning ten full pages to answer one question.
//!
//! Transport is the caller's choice (stdio is what `anamnesis mcp` uses); this
//! crate only implements [`ServerHandler`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::SessionId;
use anamnesis_core::page::{Entity, Frontmatter, Page, PagePath, PageStatus, Tier};
use anamnesis_core::retrieval::Tuning;
use anamnesis_core::scope::{OperatorName, ResolvedScope};
use anamnesis_core::session::AgentKind;
use anamnesis_core::workstream::{Workstream, WorkstreamSlug};
use anamnesis_llm::Embedder;
use anamnesis_store::{Store, StreamBreakdown, new_session};
use anamnesis_wiki::{ClearField, PagePatch, Wiki};
use jiff::Timestamp;
use parking_lot::Mutex;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{
    Json, ServerHandler, handler::server::router::tool::ToolRouter, tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Errors a tool call can fail with, always surfaced to the caller as text.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Storage failed.
    #[error("storage error: {0}")]
    Store(#[from] anamnesis_store::StoreError),
    /// The wiki failed.
    #[error("wiki error: {0}")]
    Wiki(#[from] anamnesis_wiki::WikiError),
    /// A core validation rejected the input.
    #[error(transparent)]
    Core(#[from] anamnesis_core::CoreError),
    /// The request itself was malformed in a way core validation does not cover.
    #[error("{0}")]
    Invalid(String),
}

/// Request for [`AnamnesisMcp::memory_query`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryRequest {
    /// Search text: keywords or entity names to look for across the wiki.
    pub text: String,
    /// Maximum number of pages to return. Defaults to 10, capped at 50.
    pub limit: Option<u32>,
    /// Show the working behind each score: which streams found the page, where
    /// it ranked in each, and what that was worth. Defaults to false. Costs a
    /// second pass over the streams, so it is worth asking for when a ranking
    /// looks wrong and not otherwise.
    pub explain: Option<bool>,
}

/// One page in a [`QueryResponse`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryHit {
    /// Project-relative path.
    pub path: String,
    /// Page title.
    pub title: String,
    /// Temporal tier: working, episodic, semantic, or procedural.
    pub tier: String,
    /// Trust status: active, historical, do-not-answer-from, or superseded.
    pub status: String,
    /// Exempt from decay.
    pub pinned: bool,
    /// Declared authoritative on its subject.
    pub canonical: bool,
    /// Fused relevance score. Comparable within one response, not across two.
    pub score: f64,
    /// Leading slice of the page body.
    pub snippet: String,
    /// True when the page comes from the workspace's shared `_global` scope
    /// rather than this project: something held to be true of every project
    /// here, not only of this one.
    pub global: bool,
    /// The working behind `score`, when the request asked for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<QueryExplain>,
}

/// Where one page landed in one retrieval stream, and what that was worth.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StreamRank {
    /// 1-based rank in this stream. Absent when the stream did not find the
    /// page at all, which is the more interesting half of the answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    /// What that rank contributed to the fused score: `weight / (k + rank)`,
    /// and zero when the stream missed. For the vector stream, also multiplied
    /// by `coverage` raised to the tuning's `vector_coverage` exponent.
    pub contribution: f64,
    /// Vector stream only: the share of the page its vector was embedded from.
    /// `1.0` for a page that fit the model; less for one it truncated, whose
    /// vector stands for its opening and not for the rest of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
}

/// The working behind one hit's score.
///
/// Scoring happens in two stages and this reports the first one, because the
/// first is where the arguable numbers live. Within a scope, four streams are
/// fused by weighted reciprocal rank and the total is multiplied by the page's
/// standing. Across scopes — this project and the workspace's shared one —
/// those two rankings are fused again, by plain RRF over rank alone, and
/// *that* is what `score` on the hit reports. So `within_scope` below is not
/// `score` and is not meant to be: it is the number that decided this page's
/// place in its own scope's ranking, which is the number a weight argument is
/// actually about.
#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryExplain {
    /// Which scope's streams these ranks come from: `project` or `global`.
    pub scope: String,
    /// Full-text (BM25) stream.
    pub fts: StreamRank,
    /// Declared-entity stream, ordered by inverse document frequency.
    pub entity: StreamRank,
    /// Link-neighbour stream, seeded from what full-text and entities found.
    pub links: StreamRank,
    /// Vector-cosine stream. Always a miss when no embedder is configured.
    pub vectors: StreamRank,
    /// The query vector against each page's one-line abstract. A rank here with
    /// a zero contribution is a stream that found the page and was not given
    /// weight, which is how it ships.
    pub abstracts: StreamRank,
    /// Sum of the contributions: what ordered this page within its own scope.
    pub fused: f64,
    /// The factor the page's standing — authoritative namespace, canonical,
    /// pinned, and a session page that recorded almost nothing — put on the
    /// place each rank above counted from. `1.0` means it was weighed and left
    /// alone, and a page no stream found is never surfaced however high it is.
    pub standing: f64,
    /// The same number as `fused`, kept under the name an earlier explain gave
    /// it, when standing multiplied the score instead of moving the place.
    pub within_scope: f64,
}

/// Response for [`AnamnesisMcp::memory_query`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryResponse {
    /// Matching pages, best match first.
    pub hits: Vec<QueryHit>,
}

/// Both scopes' streams, unfused, with the tuning that fused them.
///
/// Private: this is the raw material an explain is computed from, not
/// something a caller is handed.
struct StreamWorking {
    tuning: Tuning,
    project: StreamBreakdown,
    shared: StreamBreakdown,
}

impl StreamWorking {
    /// The working for one hit, read out of the scope it came from.
    fn for_hit(&self, hit: &anamnesis_store::PageHit, from_global: bool) -> QueryExplain {
        let streams = if from_global {
            &self.shared
        } else {
            &self.project
        };
        let weights = self.tuning.weights();
        // Standing decides the place each rank counts from, so it belongs
        // inside the contributions rather than beside them: an explanation
        // whose numbers do not add up to the score is worse than none.
        let standing = hit.standing;
        let rank_in = |stream: &[anamnesis_core::ids::PageId], weight: f64| {
            let rank = stream
                .iter()
                .position(|id| *id == hit.page_id)
                .map(|index| index + 1);
            StreamRank {
                rank,
                contribution: rank
                    .map(|rank| weight / (self.tuning.rrf_k + rank as f64 / standing))
                    .unwrap_or(0.0),
                coverage: None,
            }
        };

        let fts = rank_in(&streams.fts, weights[0]);
        let entity = rank_in(&streams.entity, weights[1]);
        let links = rank_in(&streams.links, weights[2]);
        // The vector stream is the one whose ranks are not all worth the same:
        // fusion scales each by how much of its page the vector read, and an
        // explanation that left that out would report a contribution the
        // ranking never used.
        let vectors = {
            let mut vectors = rank_in(&streams.vectors, weights[3]);
            if let Some(rank) = vectors.rank {
                let coverage = streams
                    .vector_coverage
                    .get(rank - 1)
                    .copied()
                    .unwrap_or(1.0);
                vectors.contribution *= self.tuning.vector_scale(coverage);
                vectors.coverage = Some(coverage);
            }
            vectors
        };
        let abstracts = rank_in(&streams.abstracts, weights[4]);
        let fused = fts.contribution
            + entity.contribution
            + links.contribution
            + vectors.contribution
            + abstracts.contribution;
        QueryExplain {
            scope: if from_global { "global" } else { "project" }.to_owned(),
            fts,
            entity,
            links,
            vectors,
            abstracts,
            fused,
            standing,
            within_scope: fused,
        }
    }
}

/// Request for [`AnamnesisMcp::memory_write_page`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WritePageRequest {
    /// Project-relative path, e.g. `decisions/0001-storage.md`. Must end in `.md`.
    pub path: String,
    /// Page title.
    pub title: String,
    /// Markdown body. `[[other-page.md]]` links are indexed for retrieval.
    pub body: String,
    /// working | episodic | semantic | procedural. Defaults to episodic.
    pub tier: Option<String>,
    /// active | historical | do-not-answer-from | superseded. Defaults to active.
    pub status: Option<String>,
    /// Exempt this page from the decay sweep. Defaults to false.
    pub pinned: Option<bool>,
    /// Declare this the authoritative page on its subject. Defaults to false.
    pub canonical: Option<bool>,
    /// Canonical names this page is about, at most 10.
    pub entities: Option<Vec<String>>,
    /// RFC 3339 timestamp after which the page should be forgotten.
    pub expires_at: Option<String>,
    /// Path of a page this one replaces.
    pub supersedes: Option<String>,
    /// Importance assigned at write time, the seed of the decay score. Defaults to 1.0.
    pub salience: Option<f64>,
}

/// Response for [`AnamnesisMcp::memory_write_page`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct WritePageResponse {
    /// Where the page was written.
    pub path: String,
    /// Git commit the write landed in.
    pub commit: String,
    /// Opaque content revision to use for a later patch.
    pub revision: String,
}

/// Request for [`AnamnesisMcp::memory_patch_page`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PatchPageRequest {
    /// Existing project-relative path.
    pub path: String,
    /// Opaque revision returned by `memory_read_page`.
    pub expected_revision: String,
    /// Replacement title. Omit to preserve it.
    pub title: Option<String>,
    /// Replacement Markdown body. Omit to preserve it.
    pub body: Option<String>,
    /// Replacement temporal tier. Omit to preserve it.
    pub tier: Option<String>,
    /// Replacement authored trust status. Omit to preserve it.
    pub status: Option<String>,
    /// Replacement pin flag. Omit to preserve it.
    pub pinned: Option<bool>,
    /// Replacement canonical flag. Omit to preserve it.
    pub canonical: Option<bool>,
    /// Replacement entities. Omit to preserve them.
    pub entities: Option<Vec<String>>,
    /// Replacement expiry. Omit to preserve it.
    pub expires_at: Option<String>,
    /// Replacement supersession target. Omit to preserve it.
    pub supersedes: Option<String>,
    /// Replacement salience. Omit to preserve it.
    pub salience: Option<f64>,
    /// Replacement one-line abstract. Omit to preserve it.
    #[serde(rename = "abstract")]
    pub page_abstract: Option<String>,
    /// Fields to remove explicitly: supersedes, expires_at, entities, abstract.
    pub clear_fields: Option<Vec<String>>,
    /// Select the workspace-shared scope instead of this project.
    pub global: Option<bool>,
}

/// One supersession-chain consequence of a patch.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ChainEffect {
    /// Page whose standing may have changed.
    pub path: String,
    /// Whether it is now the head of its chain.
    pub is_latest: bool,
}

/// Response for [`AnamnesisMcp::memory_patch_page`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct PatchPageResponse {
    /// Patched path.
    pub path: String,
    /// Git commit containing the patch.
    pub commit: String,
    /// Opaque content revision after the patch.
    pub revision: String,
    /// Fields explicitly supplied.
    pub changed: Vec<String>,
    /// Existing fields kept because they were omitted.
    pub preserved: Vec<String>,
    /// Fields removed through `clear_fields`.
    pub cleared: Vec<String>,
    /// Whether this page is the head of its chain.
    pub is_latest: bool,
    /// Newer page replacing this one, when any.
    pub superseded_by: Option<String>,
    /// Predecessor standing changes caused by clearing or changing supersedes.
    pub chain_effects: Vec<ChainEffect>,
    /// Important consequences the caller should not mistake for an ordinary edit.
    pub warnings: Vec<String>,
}

/// Request for [`AnamnesisMcp::memory_read_page`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadPageRequest {
    /// Project-relative path, exactly as `memory_query` reported it, e.g.
    /// `decisions/0001-storage.md`.
    pub path: String,
    /// Copy the hit's `global` flag: true reads only the workspace's shared
    /// scope, false only this project. Omit for legacy project-first lookup.
    pub global: Option<bool>,
}

/// Response for [`AnamnesisMcp::memory_read_page`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadPageResponse {
    /// Project-relative path the page was found at.
    pub path: String,
    /// Page title.
    pub title: String,
    /// The whole markdown body, untruncated. This is the point of the tool.
    pub body: String,
    /// Opaque content revision required for a safe patch.
    pub revision: String,
    /// Temporal tier: working, episodic, semantic, or procedural.
    pub tier: String,
    /// Trust status: active, historical, do-not-answer-from, or superseded.
    ///
    /// Worth reading before the body. A `do-not-answer-from` page is kept
    /// deliberately — it explains a contradiction a reader may already be
    /// holding — and is the one status where the body is evidence about what
    /// was once believed rather than about what is true.
    pub status: String,
    /// Effective trust state after applying the supersession chain.
    pub effective_status: String,
    /// Whether this page is the head of its supersession chain.
    pub is_latest: bool,
    /// Newer page replacing this one, when any.
    pub superseded_by: Option<String>,
    /// Exempt from decay.
    pub pinned: bool,
    /// Declared authoritative on its subject.
    pub canonical: bool,
    /// Canonical names this page declares itself to be about.
    pub entities: Vec<String>,
    /// Path of the page this one replaces, if it replaces one.
    pub supersedes: Option<String>,
    /// Importance assigned at write time, the seed of the decay score.
    pub salience: f64,
    /// RFC 3339 instant after which the page should be forgotten, if set.
    pub expires_at: Option<String>,
    /// Authored one-line abstract, when present.
    #[serde(rename = "abstract")]
    pub page_abstract: Option<String>,
    /// Session that first created this page through consolidation, if any.
    pub source_session: Option<String>,
    /// True when the page came from the workspace's shared `_global` scope
    /// rather than this project.
    pub global: bool,
}

/// Request for [`AnamnesisMcp::memory_handoff_accept`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HandoffAcceptRequest {
    /// Identifier of the session accepting the handoff.
    pub session_id: String,
    /// Which harness is asking. Defaults to `mcp` when unspecified.
    pub agent: Option<String>,
    /// Slug of the workstream to join and claim a handoff from. Must already
    /// exist (see `workstream_start`). Omitted claims the shared,
    /// workstream-less slot, same as before workstreams existed.
    pub workstream: Option<String>,
    /// Operator whose slot to claim from, where the project keys slots by
    /// operator (`[slots] per_user`). Ignored where it does not.
    ///
    /// This names a slot; it does not prove anything about who is asking.
    /// Over HTTP the bearer token settles that, but MCP is a subprocess the
    /// agent launched with the store open in front of it, and there is nobody
    /// left to authenticate to.
    pub operator: Option<String>,
}

/// Response for [`AnamnesisMcp::memory_handoff_accept`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct HandoffAcceptResponse {
    /// The pending handoff's body, if there was one to claim.
    pub handoff: Option<String>,
}

/// Request for [`AnamnesisMcp::workstream_start`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkstreamStartRequest {
    /// Stable short name, e.g. `auth-refactor`. Lowercase letters, digits,
    /// `-`, and `_` only.
    pub slug: String,
    /// Human-facing title.
    pub title: String,
}

/// Response for [`AnamnesisMcp::workstream_start`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkstreamStartResponse {
    /// The slug, normalized.
    pub slug: String,
    /// Current title.
    pub title: String,
    /// Current lifecycle status.
    pub status: String,
}

/// Request for [`AnamnesisMcp::workstream_status`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkstreamStatusRequest {
    /// Slug of the workstream to describe.
    pub slug: String,
}

/// One session in a workstream's ledger.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkstreamSessionEntry {
    /// The session's identifier.
    pub session_id: String,
    /// Which harness ran it.
    pub agent: String,
    /// Its lifecycle state.
    pub state: String,
    /// When it started.
    pub started_at: String,
    /// When it ended, if it has.
    pub ended_at: Option<String>,
}

/// One handoff in a workstream's ledger.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkstreamHandoffEntry {
    /// Delivery state: `pending`, `accepted`, or `expired`.
    pub state: String,
    /// When it was written.
    pub created_at: String,
    /// When it was accepted, if it has been.
    pub accepted_at: Option<String>,
}

/// Response for [`AnamnesisMcp::workstream_status`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkstreamStatusResponse {
    /// The slug asked for.
    pub slug: String,
    /// Current title.
    pub title: String,
    /// Current lifecycle status.
    pub status: String,
    /// Sessions that have joined this workstream, oldest first.
    pub sessions: Vec<WorkstreamSessionEntry>,
    /// Handoffs written within this workstream, newest first.
    pub handoffs: Vec<WorkstreamHandoffEntry>,
}

/// The MCP server: one resolved project scope, backed by its store and wiki.
#[derive(Clone)]
pub struct AnamnesisMcp {
    store: Arc<Store>,
    wiki: Arc<Mutex<Wiki>>,
    scope: ResolvedScope,
    root: PathBuf,
    /// When present, `memory_query` runs a fourth stream (vector-cosine over
    /// pages this embedder has embedded) and `memory_write_page` embeds every
    /// page it writes. Absent by default: nothing here requires local
    /// inference, the same way nothing requires a configured LLM provider.
    embedder: Option<Arc<dyn Embedder>>,
    tool_router: ToolRouter<Self>,
}

impl AnamnesisMcp {
    /// Build a server bound to `scope`, backed by an already-open store and
    /// wiki. `root` is recorded as the checkout path for sessions this server
    /// creates when accepting handoffs.
    pub fn new(store: Store, wiki: Wiki, scope: ResolvedScope, root: PathBuf) -> Self {
        Self {
            store: Arc::new(store),
            wiki: Arc::new(Mutex::new(wiki)),
            scope,
            root,
            embedder: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Enable the vector-cosine retrieval stream, backed by `embedder`.
    pub fn with_embedder(mut self, embedder: Option<Arc<dyn Embedder>>) -> Self {
        self.embedder = embedder;
        self
    }

    /// The scope this server is bound to.
    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }
}

// Every tool says whether it changes memory, because harnesses decide by it
// whether to ask first. A harness running unattended cannot ask anyone: on
// 2026-09-22 `codex exec` refused `memory_query` outright — "MCP tool call
// requires approval, but approval policy is never" — and the agent, left
// without its memory tools, went and grepped the wiki's files instead. With
// these hints codex-cli 0.155.1 runs the three read-only tools unasked under
// its default settings; the other four are marked as the writes they are, so
// a harness that asks before writes still asks.
#[tool_router(router = tool_router)]
impl AnamnesisMcp {
    /// Search the project's memory wiki.
    ///
    /// Fuses independent relevance signals — full-text search, entity
    /// matching, link-neighbour expansion, and (when a local embedder is
    /// configured) vector-cosine similarity — so a page every signal agrees
    /// on outranks one signal's favorite. Pinned, canonical, and authoritative
    /// pages (`decisions/`, `_rules/`, `procedures/`, `gotchas/`) are boosted
    /// among the results a query already found relevant; a page no stream
    /// finds relevant is never surfaced no matter its standing.
    #[tool(
        name = "memory_query",
        description = "Search the project's memory wiki for pages relevant to a query.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn memory_query(
        &self,
        params: Parameters<QueryRequest>,
    ) -> Result<Json<QueryResponse>, String> {
        self.query(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Create a wiki page.
    ///
    /// An existing path is rejected: updating it requires first reading its
    /// revision and then calling `memory_patch_page`, so omitted frontmatter
    /// cannot silently reset metadata and concurrent editors cannot overwrite
    /// one another.
    #[tool(
        name = "memory_write_page",
        description = "Create a new page in the project's memory wiki. Existing paths are rejected; use memory_patch_page to update one.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn memory_write_page(
        &self,
        params: Parameters<WritePageRequest>,
    ) -> Result<Json<WritePageResponse>, String> {
        self.write_page(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Patch an existing wiki page without resetting omitted metadata.
    #[tool(
        name = "memory_patch_page",
        description = "Patch an existing page after reading its revision. Omitted fields are preserved; nullable fields are removed only through clear_fields.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn memory_patch_page(
        &self,
        params: Parameters<PatchPageRequest>,
    ) -> Result<Json<PatchPageResponse>, String> {
        self.patch_page(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Read one page whole.
    ///
    /// `memory_query` returns a snippet, which is enough to decide whether a
    /// page is the right one and not enough to act on it. Without this tool an
    /// agent that has found exactly the page it needs still works from three
    /// sentences, and the usual repair — querying again with narrower words to
    /// shake loose a better snippet — asks retrieval to do a job reading does.
    ///
    /// Copy both `path` and `global` from the query hit to read that exact
    /// scope, even when the other scope holds a page at the same path.
    /// Omitting `global` preserves the legacy project-first lookup.
    #[tool(
        name = "memory_read_page",
        description = "Read one page of the memory wiki in full. Copy both path and global from the memory_query hit to select the same page.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn memory_read_page(
        &self,
        params: Parameters<ReadPageRequest>,
    ) -> Result<Json<ReadPageResponse>, String> {
        self.read_page(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Claim the pending handoff left by the previous session, if there is one.
    ///
    /// A handoff is single-use: the first session to accept it consumes it, so
    /// calling this twice with different session ids returns the note once and
    /// `null` afterward.
    #[tool(
        name = "memory_handoff_accept",
        description = "Claim the pending handoff left by the previous session, if any.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn memory_handoff_accept(
        &self,
        params: Parameters<HandoffAcceptRequest>,
    ) -> Result<Json<HandoffAcceptResponse>, String> {
        self.accept_handoff(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Start, or resume, a named workstream.
    ///
    /// A workstream is a persistent thread of work that can span many
    /// sessions and many harnesses — useful when a project has more than one
    /// thread in flight at once (`auth-refactor` alongside `bug-123`), since
    /// each gets its own pending-handoff slot and its own visible history.
    /// Calling this with a slug that already exists updates its title rather
    /// than starting a second one.
    #[tool(
        name = "workstream_start",
        description = "Start or resume a named workstream: a persistent thread of work spanning many sessions and harnesses.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn workstream_start(
        &self,
        params: Parameters<WorkstreamStartRequest>,
    ) -> Result<Json<WorkstreamStartResponse>, String> {
        self.start_workstream(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Show a workstream's status and its visible event ledger.
    ///
    /// The ledger is exactly the sessions that joined it and the handoffs
    /// written within it — nothing is summarized or synthesized, so this is
    /// a raw log of what actually happened, in order.
    #[tool(
        name = "workstream_status",
        description = "Show a workstream's status and its event ledger (sessions and handoffs).",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn workstream_status(
        &self,
        params: Parameters<WorkstreamStatusRequest>,
    ) -> Result<Json<WorkstreamStatusResponse>, String> {
        self.describe_workstream(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AnamnesisMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Long-term memory for this project. Call memory_query before starting work that \
             might already have prior decisions, gotchas, or context recorded. It returns \
             snippets: when a hit looks like the answer, call memory_read_page with its path and global flag \
             to read the page in full rather than querying again with different words — a \
             snippet is enough to choose a page and not enough to act on one. Call \
             memory_write_page to create durable knowledge — decisions, gotchas, procedures — \
             worth keeping past this session. To update an existing page, read its revision and \
             call memory_patch_page; omitted fields are preserved there. Ordinary session summaries are written \
             automatically and do not need this tool. If this project has more than one \
             thread of work going at once, call workstream_start with a short slug before \
             memory_handoff_accept and name that slug there too — each workstream keeps its \
             own resume point, so switching between them never loses or shadows the other's \
             handoff. Use workstream_status to see what happened in one.",
        )
    }
}

impl AnamnesisMcp {
    fn query(&self, request: QueryRequest) -> Result<QueryResponse, McpError> {
        let limit = request.limit.unwrap_or(10).clamp(1, 50) as usize;

        // A broken or slow-to-load embedder costs this query its fourth
        // stream, not the whole search — the other three still run, the same
        // way a refused LLM call still leaves the deterministic page.
        let query_vector = self.embedder.as_ref().and_then(|embedder| {
            match embedder.embed(&request.text) {
                Ok(vector) => Some((embedder.model().to_owned(), vector)),
                Err(error) => {
                    tracing::warn!(%error, "query embedding failed; searching without the vector stream");
                    None
                }
            }
        });

        // The workspace's shared scope is searched alongside this project's,
        // so a policy written once is answerable from every project under it.
        let global = self.global_scope();
        let embedding = query_vector
            .as_ref()
            .map(|(model, vector)| (model.as_str(), vector.as_slice()));
        let hits = self.store.query_pages_across(
            self.scope.project_id,
            &[global.project_id],
            &request.text,
            limit,
            Timestamp::now(),
            embedding,
        )?;

        // Re-run the streams unfused, only when asked. `query_streams` is the
        // diagnostic path precisely because it records no access: asking which
        // stream *would have* found a page is not the same as handing the page
        // over, and the decay sweep reads those counters.
        let working = if request.explain.unwrap_or(false) {
            Some(self.stream_working(&request.text, embedding, &global)?)
        } else {
            None
        };

        Ok(QueryResponse {
            hits: hits
                .into_iter()
                .map(|hit| {
                    let from_global = hit.project_id == global.project_id;
                    let explain = working
                        .as_ref()
                        .map(|working| working.for_hit(&hit, from_global));
                    QueryHit {
                        path: hit.path.as_str().to_owned(),
                        title: hit.title,
                        tier: hit.tier.as_str().to_owned(),
                        status: hit.status.as_str().to_owned(),
                        pinned: hit.pinned,
                        canonical: hit.canonical,
                        score: hit.score,
                        snippet: hit.snippet,
                        global: from_global,
                        explain,
                    }
                })
                .collect(),
        })
    }

    /// Run both scopes' streams unfused, at the depth the real search uses.
    ///
    /// Depth is `tuning.candidates` rather than the caller's `limit`: the
    /// ranking being explained was fused from that many candidates per stream,
    /// and a shallower re-run would report a page as "not found by full text"
    /// when full text found it twelfth.
    fn stream_working(
        &self,
        text: &str,
        embedding: Option<(&str, &[f32])>,
        global: &ResolvedScope,
    ) -> Result<StreamWorking, McpError> {
        let tuning = Tuning::default();
        let project = self.store.query_streams(
            self.scope.project_id,
            text,
            tuning.candidates,
            embedding,
            &tuning,
        )?;
        // The shared scope is skipped when this server *is* it, exactly as the
        // search itself skips it, so the two cannot disagree about what ran.
        let shared = if global.project_id == self.scope.project_id {
            StreamBreakdown::default()
        } else {
            self.store.query_streams(
                global.project_id,
                text,
                tuning.candidates,
                embedding,
                &tuning,
            )?
        };
        Ok(StreamWorking {
            tuning,
            project,
            shared,
        })
    }

    /// Record a change an agent made, so a person can find out later that it
    /// was an agent that made it.
    ///
    /// Never fails the call: the page is already written and committed by the
    /// time this runs, and refusing the tool call now would tell the model
    /// something false about what happened.
    fn note(
        &self,
        action: anamnesis_core::audit::Action,
        subject: impl Into<String>,
        detail: Option<String>,
        operator: Option<OperatorName>,
    ) {
        let mut entry = anamnesis_core::audit::AuditEntry::new(
            action,
            anamnesis_core::audit::Via::Mcp,
            subject,
            Timestamp::now(),
        )
        .in_project(self.scope.project_id)
        .by(operator);
        if let Some(detail) = detail {
            entry = entry.saying(detail);
        }
        if let Err(error) = self.store.append_audit(&entry) {
            tracing::warn!(%error, "a change was made but not recorded in the audit log");
        }
    }

    fn write_page(&self, request: WritePageRequest) -> Result<WritePageResponse, McpError> {
        let path = PagePath::parse(&request.path)?;
        let entities = request
            .entities
            .unwrap_or_default()
            .iter()
            .map(|name| Entity::parse(name))
            .collect::<anamnesis_core::Result<Vec<_>>>()?;

        let mut frontmatter = Frontmatter::new(&request.title, entities.clone())?;
        frontmatter.tier = parse_tier(request.tier.as_deref())?;
        frontmatter.status = parse_status(request.status.as_deref())?;
        frontmatter.pinned = request.pinned.unwrap_or(false);
        frontmatter.canonical = request.canonical.unwrap_or(false);
        if let Some(salience) = request.salience {
            frontmatter.salience = salience;
        }
        if let Some(supersedes) = &request.supersedes {
            frontmatter.supersedes = Some(PagePath::parse(supersedes)?);
        }
        if let Some(expires_at) = &request.expires_at {
            frontmatter.expires_at = Some(expires_at.parse().map_err(|_| {
                McpError::Invalid(format!("expires_at {expires_at:?} is not RFC 3339"))
            })?);
        }

        let mut page = Page::new(
            self.scope.project_id,
            path.clone(),
            frontmatter,
            request.body.clone(),
        );
        let written = {
            let wiki = self.wiki.lock();
            match wiki.create_page(&self.scope.scope, &page, &format!("mcp: create {path}")) {
                Ok(written) => written,
                Err(anamnesis_wiki::WikiError::AlreadyExists(_)) => {
                    let current = wiki.read_versioned_page(&self.scope.scope, &path)?;
                    let current_frontmatter = serde_json::to_string(&current.parsed.frontmatter)
                        .unwrap_or_else(|_| "<could not serialize frontmatter>".to_owned());
                    let replacement = self
                        .store
                        .superseded_by(self.scope.project_id, &path)?
                        .map(|newer| format!("; this page was superseded by {newer}"))
                        .unwrap_or_default();
                    return Err(McpError::Invalid(format!(
                        "page {path} already exists at revision {}{replacement}; current frontmatter: {current_frontmatter}. Read it with memory_read_page, then use memory_patch_page with expected_revision",
                        current.revision
                    )));
                }
                Err(error) => return Err(error.into()),
            }
        };
        let commit = written.commit;
        page.git_commit = Some(commit.clone());
        self.note(
            anamnesis_core::audit::Action::PageWritten,
            path.to_string(),
            Some(format!("commit {}", &commit[..commit.len().min(8)])),
            None,
        );

        let now = Timestamp::now();
        self.store.upsert_project(&self.scope, now)?;
        // Row, entities, links and — when one is configured — the vector, in
        // the one call every writer makes. That call also records a failed or
        // truncated embedding, and an embedding failure costs the page its
        // place in the vector stream rather than the write. There used to be a
        // second embedding here, after it, which recorded nothing: every write
        // paid the model twice, and a first attempt that failed followed by a
        // second that did not left a vector beside the complaint that there
        // was none.
        let links = anamnesis_wiki::extract_links(&request.body);
        self.store.index_page(
            self.scope.project_id,
            &page,
            &links,
            self.embedder
                .as_deref()
                .map(|embedder| embedder as &dyn anamnesis_core::embedding::Embed),
            now,
        )?;

        Ok(WritePageResponse {
            path: path.as_str().to_owned(),
            commit,
            revision: written.revision,
        })
    }

    fn patch_page(&self, request: PatchPageRequest) -> Result<PatchPageResponse, McpError> {
        let path = PagePath::parse(&request.path)?;
        let target = if request.global.unwrap_or(false) {
            self.global_scope()
        } else {
            self.scope.clone()
        };

        let entities = request
            .entities
            .as_ref()
            .map(|names| {
                names
                    .iter()
                    .map(|name| Entity::parse(name))
                    .collect::<anamnesis_core::Result<Vec<_>>>()
            })
            .transpose()?;
        let expires_at = request
            .expires_at
            .as_ref()
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| McpError::Invalid(format!("expires_at {value:?} is not RFC 3339")))
            })
            .transpose()?;
        let mut clear = BTreeSet::new();
        for name in request.clear_fields.unwrap_or_default() {
            let field = match name.as_str() {
                "supersedes" => ClearField::Supersedes,
                "expires_at" => ClearField::ExpiresAt,
                "entities" => ClearField::Entities,
                "abstract" => ClearField::Abstract,
                _ => {
                    return Err(McpError::Invalid(format!(
                        "clear_fields contains {name:?}; allowed fields are supersedes, expires_at, entities, abstract"
                    )));
                }
            };
            clear.insert(field);
        }
        let patch = PagePatch {
            title: request.title,
            body: request.body,
            tier: request
                .tier
                .as_deref()
                .map(|value| parse_tier(Some(value)))
                .transpose()?,
            status: request
                .status
                .as_deref()
                .map(|value| parse_status(Some(value)))
                .transpose()?,
            pinned: request.pinned,
            canonical: request.canonical,
            entities,
            expires_at,
            supersedes: request
                .supersedes
                .as_deref()
                .map(PagePath::parse)
                .transpose()?,
            salience: request.salience,
            page_abstract: request.page_abstract,
            clear,
        };

        let mut written = {
            let wiki = self.wiki.lock();
            wiki.patch_page(
                &target.scope,
                target.project_id,
                &path,
                &request.expected_revision,
                &patch,
                &format!("mcp: patch {path}"),
            )?
        };
        written.page.git_commit = Some(written.commit.clone());
        let now = Timestamp::now();
        self.store.upsert_project(&target, now)?;
        let links = anamnesis_wiki::extract_links(&written.page.body);
        self.store.index_page(
            target.project_id,
            &written.page,
            &links,
            self.embedder
                .as_deref()
                .map(|embedder| embedder as &dyn anamnesis_core::embedding::Embed),
            now,
        )?;
        self.note(
            anamnesis_core::audit::Action::PageWritten,
            path.to_string(),
            Some(format!(
                "patch commit {}",
                &written.commit[..written.commit.len().min(8)]
            )),
            None,
        );

        let is_latest = self
            .store
            .page_is_latest(target.project_id, &path)?
            .unwrap_or(true);
        let superseded_by = self
            .store
            .superseded_by(target.project_id, &path)?
            .map(|newer| newer.as_str().to_owned());
        let mut affected = BTreeSet::new();
        if let Some(previous) = &written.previous_supersedes {
            affected.insert(previous.clone());
        }
        if let Some(current) = &written.page.frontmatter.supersedes {
            affected.insert(current.clone());
        }
        let chain_effects = affected
            .into_iter()
            .map(|affected_path| {
                Ok(ChainEffect {
                    is_latest: self
                        .store
                        .page_is_latest(target.project_id, &affected_path)?
                        .unwrap_or(true),
                    path: affected_path.as_str().to_owned(),
                })
            })
            .collect::<Result<Vec<_>, McpError>>()?;
        let mut warnings = Vec::new();
        if let Some(newer) = &superseded_by {
            warnings.push(format!(
                "this page remains retired; it was superseded by {newer}"
            ));
        }
        if written.cleared.iter().any(|field| field == "supersedes") {
            for effect in &chain_effects {
                if effect.is_latest {
                    warnings.push(format!(
                        "clearing supersedes made {} a chain head again",
                        effect.path
                    ));
                }
            }
        }

        Ok(PatchPageResponse {
            path: path.as_str().to_owned(),
            commit: written.commit,
            revision: written.revision,
            changed: written.changed,
            preserved: written.preserved,
            cleared: written.cleared,
            is_latest,
            superseded_by,
            chain_effects,
            warnings,
        })
    }

    fn read_page(&self, request: ReadPageRequest) -> Result<ReadPageResponse, McpError> {
        let path = PagePath::parse(&request.path)?;

        // An explicit scope must not fall through: a query may have found the
        // shared policy while this project holds a different page at its path.
        // Keep project-first fallback only for callers of the original API.
        let global = self.global_scope();
        let (versioned, scope_project_id, from_global) = match request.global {
            Some(from_global) => {
                let scope = if from_global { &global } else { &self.scope };
                let versioned = self.read_from_scope(&scope.scope, &path)?.ok_or_else(|| {
                    McpError::Invalid(format!(
                        "no page at {path} in {}",
                        if from_global {
                            "the workspace's shared scope"
                        } else {
                            "this project"
                        }
                    ))
                })?;
                (versioned, scope.project_id, from_global)
            }
            None => match self.read_from_scope(&self.scope.scope, &path)? {
                Some(versioned) => (versioned, self.scope.project_id, false),
                None => match self.read_from_scope(&global.scope, &path)? {
                    Some(versioned) => (versioned, global.project_id, true),
                    None => {
                        return Err(McpError::Invalid(format!(
                            "no page at {path} in this project or the workspace's shared scope"
                        )));
                    }
                },
            },
        };

        // Reading a page is using it, which is the same thing the decay sweep
        // reads `access_count` to find out — so a page an agent opens and acts
        // on is renewed exactly as one a query surfaced is. Best-effort: a
        // page present on disk but absent from the index is still a page, and
        // the wiki is what decides that.
        let page_id = anamnesis_core::ids::PageId::derive(scope_project_id, &path);
        if let Err(error) = self.store.record_access(page_id, Timestamp::now()) {
            tracing::warn!(%error, %path, "page was read but its access was not recorded");
        }

        let is_latest = self
            .store
            .page_is_latest(scope_project_id, &path)?
            .unwrap_or(true);
        let superseded_by = self
            .store
            .superseded_by(scope_project_id, &path)?
            .map(|newer| newer.as_str().to_owned());
        let revision = versioned.revision;
        let parsed = versioned.parsed;
        let frontmatter = parsed.frontmatter;
        let authored_status = frontmatter.status.as_str().to_owned();
        Ok(ReadPageResponse {
            path: path.as_str().to_owned(),
            title: frontmatter.title,
            body: parsed.body,
            revision,
            tier: frontmatter.tier.as_str().to_owned(),
            status: authored_status.clone(),
            effective_status: if is_latest {
                authored_status
            } else {
                "superseded".to_owned()
            },
            is_latest,
            superseded_by,
            pinned: frontmatter.pinned,
            canonical: frontmatter.canonical,
            entities: frontmatter
                .entities
                .iter()
                .map(|entity| entity.as_str().to_owned())
                .collect(),
            supersedes: frontmatter
                .supersedes
                .map(|target| target.as_str().to_owned()),
            salience: frontmatter.salience,
            expires_at: frontmatter.expires_at.map(|at| at.to_string()),
            page_abstract: frontmatter.page_abstract,
            source_session: frontmatter.session.map(|id| id.to_string()),
            global: from_global,
        })
    }

    /// Read a page from one scope, distinguishing "not there" from "broken".
    ///
    /// A missing file is how the project scope reports that the page belongs
    /// to the shared one, so it has to be a `None` rather than an error; a
    /// malformed or unreadable file is a real failure and stays one, because
    /// silently treating it as absent would send the caller looking in the
    /// other scope for a page that is right here and damaged.
    fn read_from_scope(
        &self,
        scope: &anamnesis_core::scope::Scope,
        path: &PagePath,
    ) -> Result<Option<anamnesis_wiki::VersionedPage>, McpError> {
        let wiki = self.wiki.lock();
        match wiki.read_versioned_page(scope, path) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(anamnesis_wiki::WikiError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn accept_handoff(
        &self,
        request: HandoffAcceptRequest,
    ) -> Result<HandoffAcceptResponse, McpError> {
        let agent: AgentKind = request
            .agent
            .as_deref()
            .unwrap_or("mcp")
            .parse()
            .expect("AgentKind parsing is infallible");

        let now = Timestamp::now();
        self.store.upsert_project(&self.scope, now)?;

        let workstream_id = match request.workstream.as_deref().map(str::trim) {
            Some(slug) => Some(self.require_workstream(slug)?.id),
            None => None,
        };

        let operator = match request.operator.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => Some(OperatorName::parse(name)?),
            _ => None,
        };

        let claimant = SessionId::derive(self.scope.project_id, &request.session_id);
        self.store.ensure_session(
            &new_session(
                claimant,
                self.scope.project_id,
                self.scope.workspace_id,
                agent,
                self.root.clone(),
                now,
                workstream_id,
            )
            .with_operator(operator.clone()),
        )?;

        // Gated on the project asking for per-operator slots, the same way the
        // HTTP path is: an operator named for a project that keys one slot
        // would otherwise claim from a slot nothing ever writes to.
        let slot = Slot::for_workstream(workstream_id)
            .for_operator(self.scope.slots.per_user.then_some(operator).flatten());
        let handoff = self
            .store
            .claim_handoff(self.scope.project_id, claimant, &slot, now)?;

        // Only when there was one to take. A session that asked and found
        // nothing changed nothing, and a log full of those is a log nobody
        // reads on the day one line in it matters.
        if handoff.is_some() {
            self.note(
                anamnesis_core::audit::Action::HandoffClaimed,
                claimant.to_string(),
                request
                    .workstream
                    .clone()
                    .map(|slug| format!("workstream {slug}")),
                slot.operator.clone(),
            );
        }

        Ok(HandoffAcceptResponse { handoff })
    }

    fn start_workstream(
        &self,
        request: WorkstreamStartRequest,
    ) -> Result<WorkstreamStartResponse, McpError> {
        let slug = WorkstreamSlug::parse(&request.slug)?;
        let now = Timestamp::now();
        self.store.upsert_project(&self.scope, now)?;

        let workstream = match self
            .store
            .find_workstream(self.scope.project_id, slug.as_str())?
        {
            Some(mut existing) => {
                existing.title = request.title;
                existing.updated_at = now;
                existing
            }
            None => Workstream::new(self.scope.project_id, slug, request.title, now),
        };
        self.store.upsert_workstream(&workstream)?;

        Ok(WorkstreamStartResponse {
            slug: workstream.slug.as_str().to_owned(),
            title: workstream.title,
            status: workstream.status.as_str().to_owned(),
        })
    }

    fn describe_workstream(
        &self,
        request: WorkstreamStatusRequest,
    ) -> Result<WorkstreamStatusResponse, McpError> {
        let workstream = self.require_workstream(request.slug.trim())?;

        let sessions = self
            .store
            .workstream_sessions(workstream.id)?
            .into_iter()
            .map(|session| WorkstreamSessionEntry {
                session_id: session.session_id.to_string(),
                agent: session.agent,
                state: session.state,
                started_at: session.started_at.to_string(),
                ended_at: session.ended_at.map(|t| t.to_string()),
            })
            .collect();
        let handoffs = self
            .store
            .workstream_handoffs(workstream.id)?
            .into_iter()
            .map(|handoff| WorkstreamHandoffEntry {
                state: handoff.state,
                created_at: handoff.created_at.to_string(),
                accepted_at: handoff.accepted_at.map(|t| t.to_string()),
            })
            .collect();

        Ok(WorkstreamStatusResponse {
            slug: workstream.slug.as_str().to_owned(),
            title: workstream.title,
            status: workstream.status.as_str().to_owned(),
            sessions,
            handoffs,
        })
    }

    /// Look up a workstream by slug, or report that it needs to be started
    /// first — accepting a handoff into, or asking the status of, a
    /// workstream nobody started is a caller mistake worth naming rather
    /// than silently creating one.
    /// The workspace-wide scope this project inherits from.
    ///
    /// Derived rather than looked up, so it names the same rows here as it
    /// does in the CLI. Its root is where its pages live: there is no
    /// repository behind it.
    fn global_scope(&self) -> ResolvedScope {
        self.wiki.lock().global_scope(&self.scope.scope.workspace)
    }

    fn require_workstream(&self, slug: &str) -> Result<Workstream, McpError> {
        self.store
            .find_workstream(self.scope.project_id, slug)?
            .ok_or_else(|| {
                McpError::Invalid(format!(
                    "no workstream named {slug:?}; call workstream_start first"
                ))
            })
    }
}

/// Parse a tier name from a request, rejecting anything unrecognised rather
/// than silently falling back — a typo in a tool call should fail loudly, not
/// file the page under the wrong tier.
fn parse_tier(value: Option<&str>) -> Result<Tier, McpError> {
    match value {
        None => Ok(Tier::default()),
        Some(value) => Tier::parse(value).map_err(Into::into),
    }
}

/// Parse a status name from a request. See [`parse_tier`] for why unknown
/// values are rejected rather than defaulted.
fn parse_status(value: Option<&str>) -> Result<PageStatus, McpError> {
    match value {
        None => Ok(PageStatus::default()),
        Some(value) => PageStatus::parse(value).map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::scope::resolve_scope;

    fn harness() -> (tempfile::TempDir, tempfile::TempDir, AnamnesisMcp) {
        let repo = tempfile::tempdir().expect("repo dir");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");

        let data = tempfile::tempdir().expect("data dir");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");

        let server = AnamnesisMcp::new(store, wiki, scope, repo.path().to_path_buf());
        (repo, data, server)
    }

    #[test]
    fn writing_a_page_makes_it_queryable() {
        let (_repo, _data, server) = harness();

        let written = server
            .write_page(WritePageRequest {
                path: "decisions/0001-storage.md".to_owned(),
                title: "Storage engine".to_owned(),
                body: "We chose SQLite because the index is rebuildable.".to_owned(),
                tier: Some("semantic".to_owned()),
                status: None,
                pinned: Some(true),
                canonical: None,
                entities: Some(vec!["SQLite".to_owned()]),
                expires_at: None,
                supersedes: None,
                salience: None,
            })
            .expect("write");
        assert_eq!(written.path, "decisions/0001-storage.md");
        assert!(!written.commit.is_empty());

        let found = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: None,
            })
            .expect("query");
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].title, "Storage engine");
        assert!(found.hits[0].pinned);
    }

    #[test]
    fn an_unknown_tier_is_rejected_rather_than_defaulted() {
        let (_repo, _data, server) = harness();
        let result = server.write_page(WritePageRequest {
            path: "notes/a.md".to_owned(),
            title: "A".to_owned(),
            body: "body".to_owned(),
            tier: Some("legendary".to_owned()),
            status: None,
            pinned: None,
            canonical: None,
            entities: None,
            expires_at: None,
            supersedes: None,
            salience: None,
        });
        // Asserted on the message rather than the variant: the tier names are
        // core's to know, so the refusal now arrives as `Core`. What the caller
        // gets either way is the string, and the string has to name the four
        // words that would have worked.
        let error = result
            .expect_err("an unknown tier is not a tier")
            .to_string();
        assert!(error.contains("legendary"), "{error}");
        assert!(error.contains("procedural"), "{error}");
    }

    #[test]
    fn a_handoff_is_claimed_once() {
        let (_repo, _data, server) = harness();
        let now = Timestamp::now();
        server.store.upsert_project(&server.scope, now).unwrap();
        let from = SessionId::derive(server.scope.project_id, "writer");
        server
            .store
            .ensure_session(&new_session(
                from,
                server.scope.project_id,
                server.scope.workspace_id,
                AgentKind::ClaudeCode,
                server.root.clone(),
                now,
                None,
            ))
            .unwrap();
        server
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                server.scope.project_id,
                from,
                Slot::shared(),
                "carry on",
                now,
            ))
            .unwrap();

        let first = server
            .accept_handoff(HandoffAcceptRequest {
                session_id: "reader".to_owned(),
                agent: None,
                workstream: None,
                operator: None,
            })
            .unwrap();
        assert_eq!(first.handoff.as_deref(), Some("carry on"));

        let second = server
            .accept_handoff(HandoffAcceptRequest {
                session_id: "reader-2".to_owned(),
                agent: Some("codex".to_owned()),
                workstream: None,
                operator: None,
            })
            .unwrap();
        assert_eq!(second.handoff, None);
    }

    #[test]
    fn tool_router_lists_every_tool() {
        let (_repo, _data, server) = harness();
        let tools = server.tool_router.list_all();
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();

        // Exact, not `contains`: the name of this test claims every tool, and
        // a set that only grows would pass while a tool went missing from the
        // router — which is a tool the model silently stops being offered.
        assert_eq!(
            names,
            [
                "memory_handoff_accept",
                "memory_patch_page",
                "memory_query",
                "memory_read_page",
                "memory_write_page",
                "workstream_start",
                "workstream_status",
            ]
        );
    }

    #[test]
    fn starting_a_workstream_twice_updates_rather_than_duplicates() {
        let (_repo, _data, server) = harness();
        server
            .start_workstream(WorkstreamStartRequest {
                slug: "Auth-Refactor".to_owned(),
                title: "Auth refactor".to_owned(),
            })
            .unwrap();
        let second = server
            .start_workstream(WorkstreamStartRequest {
                slug: "auth-refactor".to_owned(),
                title: "Auth refactor v2".to_owned(),
            })
            .unwrap();

        assert_eq!(second.slug, "auth-refactor");
        assert_eq!(second.title, "Auth refactor v2");
        assert_eq!(second.status, "active");
    }

    #[test]
    fn asking_the_status_of_an_unstarted_workstream_is_reported() {
        let (_repo, _data, server) = harness();
        let error = server
            .describe_workstream(WorkstreamStatusRequest {
                slug: "never-started".to_owned(),
            })
            .unwrap_err();
        assert!(matches!(error, McpError::Invalid(_)));
    }

    #[test]
    fn a_workstream_accepted_handoff_shows_up_in_its_own_ledger() {
        let (_repo, _data, server) = harness();
        server
            .start_workstream(WorkstreamStartRequest {
                slug: "auth-refactor".to_owned(),
                title: "Auth refactor".to_owned(),
            })
            .unwrap();

        let now = Timestamp::now();
        server.store.upsert_project(&server.scope, now).unwrap();
        let writer = SessionId::derive(server.scope.project_id, "writer");
        let workstream_id = server
            .store
            .find_workstream(server.scope.project_id, "auth-refactor")
            .unwrap()
            .unwrap()
            .id;
        server
            .store
            .ensure_session(&new_session(
                writer,
                server.scope.project_id,
                server.scope.workspace_id,
                AgentKind::ClaudeCode,
                server.root.clone(),
                now,
                Some(workstream_id),
            ))
            .unwrap();
        server
            .store
            .record_handoff(&anamnesis_store::new_handoff(
                server.scope.project_id,
                writer,
                Slot::for_workstream(Some(workstream_id)),
                "postgres chosen for auth storage",
                now,
            ))
            .unwrap();

        // A plain accept (no workstream named) must not see this handoff —
        // it lives in the auth-refactor slot, not the shared one.
        let plain = server
            .accept_handoff(HandoffAcceptRequest {
                session_id: "codex-reader".to_owned(),
                agent: Some("codex".to_owned()),
                workstream: None,
                operator: None,
            })
            .unwrap();
        assert_eq!(plain.handoff, None);

        let claimed = server
            .accept_handoff(HandoffAcceptRequest {
                session_id: "codex-reader-2".to_owned(),
                agent: Some("codex".to_owned()),
                workstream: Some("auth-refactor".to_owned()),
                operator: None,
            })
            .unwrap();
        assert_eq!(
            claimed.handoff.as_deref(),
            Some("postgres chosen for auth storage")
        );

        let status = server
            .describe_workstream(WorkstreamStatusRequest {
                slug: "auth-refactor".to_owned(),
            })
            .unwrap();
        assert_eq!(
            status.sessions.len(),
            2,
            "the writer and the claimant both joined"
        );
        assert_eq!(status.handoffs.len(), 1);
        assert_eq!(status.handoffs[0].state, "accepted");
    }

    #[test]
    fn accepting_into_an_unstarted_workstream_is_reported() {
        let (_repo, _data, server) = harness();
        let error = server
            .accept_handoff(HandoffAcceptRequest {
                session_id: "reader".to_owned(),
                agent: None,
                workstream: Some("never-started".to_owned()),
                operator: None,
            })
            .unwrap_err();
        assert!(matches!(error, McpError::Invalid(_)));
    }

    /// Always embeds to the same vector, regardless of text. Real semantic
    /// quality is proven once, against the real model, in
    /// `anamnesis_llm::embed::tests::the_default_model_produces_sane_normalized_vectors`;
    /// what this fake exists to prove is that the plumbing between an
    /// `Embedder`, `write_page`, and `query` actually runs.
    struct FakeEmbedder;

    impl Embedder for FakeEmbedder {
        fn dimension(&self) -> usize {
            2
        }
    }

    impl anamnesis_core::embedding::Embed for FakeEmbedder {
        fn model(&self) -> &str {
            "fake-embed-1"
        }
        fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
            Ok(vec![1.0, 0.0])
        }
    }

    /// Reads a fixed number of words and says so, the way the local model
    /// reads a fixed number of tokens.
    struct NarrowEmbedder {
        budget: usize,
    }

    impl Embedder for NarrowEmbedder {
        fn dimension(&self) -> usize {
            2
        }
    }

    impl anamnesis_core::embedding::Embed for NarrowEmbedder {
        fn model(&self) -> &str {
            "narrow-embed-1"
        }
        fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
            Ok(vec![1.0, 0.0])
        }
        fn overflow(&self, text: &str) -> Option<anamnesis_core::embedding::Overflow> {
            let tokens = text.split_whitespace().count();
            (tokens > self.budget).then_some(anamnesis_core::embedding::Overflow {
                tokens,
                budget: self.budget,
            })
        }
    }

    /// The explanation reports what the ranking used, or it is a second
    /// opinion about the ranking. A vector embedded from a quarter of its page
    /// is scaled by fusion; the working has to carry the same quarter and the
    /// same scaled contribution, whatever the shipped exponent is.
    #[test]
    fn the_working_reports_what_a_partial_vector_was_worth() {
        let (_repo, _data, server) = harness();
        let server = server.with_embedder(Some(
            Arc::new(NarrowEmbedder { budget: 10 }) as Arc<dyn Embedder>
        ));
        // One word of title and thirty-nine of body: forty words, ten read.
        write_page(&server, "notes/long.md", "Long", &"word ".repeat(39));

        let found = server
            .query(QueryRequest {
                text: "quarterly filing paperwork".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");
        let working = found.hits[0].explain.as_ref().expect("explain");

        assert_eq!(working.vectors.rank, Some(1));
        let coverage = working
            .vectors
            .coverage
            .expect("a vector rank carries the share its vector read");
        assert!((coverage - 0.25).abs() < 1e-12, "{coverage}");

        let tuning = Tuning::default();
        let expected = tuning.vectors * tuning.vector_scale(coverage) / (tuning.rrf_k + 1.0);
        assert!(
            (working.vectors.contribution - expected).abs() < 1e-12,
            "{working:?}"
        );
        assert!(
            (working.fused - working.vectors.contribution).abs() < 1e-12,
            "only the vector stream found this page: {working:?}"
        );
        assert_eq!(working.fts.coverage, None, "only the vector stream has one");
    }

    struct BrokenEmbedder;

    impl Embedder for BrokenEmbedder {
        fn dimension(&self) -> usize {
            2
        }
    }

    impl anamnesis_core::embedding::Embed for BrokenEmbedder {
        fn model(&self) -> &str {
            "broken"
        }
        fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
            Err("boom".to_owned())
        }
    }

    fn write_page(server: &AnamnesisMcp, path: &str, title: &str, body: &str) -> WritePageResponse {
        server
            .write_page(WritePageRequest {
                path: path.to_owned(),
                title: title.to_owned(),
                body: body.to_owned(),
                tier: None,
                status: None,
                pinned: None,
                canonical: None,
                entities: None,
                expires_at: None,
                supersedes: None,
                salience: None,
            })
            .expect("write")
    }

    #[test]
    fn a_page_is_findable_through_the_vector_stream_alone() {
        let (_repo, _data, server) = harness();
        let server = server.with_embedder(Some(Arc::new(FakeEmbedder) as Arc<dyn Embedder>));

        write_page(
            &server,
            "notes/car.md",
            "Automobile",
            "A vehicle with four wheels.",
        );

        // Shares no token and no entity with the page above — only the
        // (fake, constant) vector stream can connect the two.
        let found = server
            .query(QueryRequest {
                text: "quarterly filing paperwork".to_owned(),
                limit: None,
                explain: None,
            })
            .expect("query");
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].title, "Automobile");
    }

    #[test]
    fn a_broken_embedder_costs_the_vector_stream_not_the_whole_call() {
        let (_repo, _data, server) = harness();
        let server = server.with_embedder(Some(Arc::new(BrokenEmbedder) as Arc<dyn Embedder>));

        let written = write_page(&server, "notes/a.md", "A", "sqlite content");
        assert_eq!(written.path, "notes/a.md");

        let found = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: None,
            })
            .expect("query should still succeed via the other streams");
        assert_eq!(found.hits.len(), 1);
    }

    /// Fails its first call and succeeds after that, counting every call.
    struct FlakyEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Embedder for FlakyEmbedder {
        fn dimension(&self) -> usize {
            2
        }
    }

    impl anamnesis_core::embedding::Embed for FlakyEmbedder {
        fn model(&self) -> &str {
            "flaky-embed-1"
        }
        fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
            match self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => Err("the model was still loading".to_owned()),
                _ => Ok(vec![1.0, 0.0]),
            }
        }
    }

    /// A page written through this tool used to be embedded twice: once by
    /// `index_page`, which records what happened, and again by a block after
    /// it, which recorded nothing. Twice the model's time on every write, and
    /// when the first attempt failed and the second did not, a page holding a
    /// vector *and* the complaint that it had none — which `doctor` reports as
    /// broken. One call, and whatever it says is what the index says.
    #[test]
    fn a_written_page_is_embedded_once_and_the_index_agrees_with_it() {
        let (_repo, _data, server) = harness();
        let embedder = Arc::new(FlakyEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let server = server.with_embedder(Some(embedder.clone() as Arc<dyn Embedder>));

        write_page(&server, "notes/a.md", "A", "sqlite content");

        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one write, one embedding"
        );
        let complaints = server
            .store
            .embed_failures(server.scope.project_id)
            .expect("failures");
        assert_eq!(complaints.len(), 1, "the failed attempt is on record");
        let found = server
            .query(QueryRequest {
                text: "an unrelated question".to_owned(),
                limit: None,
                explain: None,
            })
            .expect("query");
        assert!(
            found.hits.is_empty(),
            "a page recorded as having no vector must not be found by one"
        );
    }

    /// The body the tool returns is the whole file, not the prefix a query
    /// would have shown — which is the entire reason the tool exists.
    #[test]
    fn reading_a_page_returns_more_than_its_snippet() {
        let (_repo, _data, server) = harness();

        // Long enough that any snippet has to cut it, and shaped so the cut
        // is detectable: the sentence that matters is at the end.
        let body = format!(
            "{}\n\nThe decision was to keep the index rebuildable.",
            "Background that goes on for a while. ".repeat(40)
        );
        write_page(&server, "decisions/0001-storage.md", "Storage", &body);

        let hit = server
            .query(QueryRequest {
                text: "rebuildable".to_owned(),
                limit: None,
                explain: None,
            })
            .expect("query");
        let snippet = &hit.hits[0].snippet;

        let page = server
            .read_page(ReadPageRequest {
                path: "decisions/0001-storage.md".to_owned(),
                global: None,
            })
            .expect("read");

        // The document the wiki writes ends in a newline whatever the caller
        // passed, so the round trip is equal up to that one character.
        assert_eq!(page.body.trim_end(), body);
        assert!(page.body.len() > snippet.len());
        assert!(
            page.body
                .trim_end()
                .ends_with("keep the index rebuildable."),
            "the tail of the page is what the snippet was dropping"
        );
        assert_eq!(page.title, "Storage");
        assert_eq!(page.tier, "episodic");
        assert_eq!(page.status, "active");
        assert!(!page.global);
    }

    /// Frontmatter travels with the body, because status changes how the body
    /// should be read: a `do-not-answer-from` page is evidence about what was
    /// believed, not about what is true.
    #[test]
    fn reading_a_page_carries_the_frontmatter_that_qualifies_it() {
        let (_repo, _data, server) = harness();

        server
            .write_page(WritePageRequest {
                path: "gotchas/stale.md".to_owned(),
                title: "The old limit".to_owned(),
                body: "We believed the cap was 512.".to_owned(),
                tier: Some("semantic".to_owned()),
                status: Some("do-not-answer-from".to_owned()),
                pinned: Some(true),
                canonical: Some(true),
                entities: Some(vec!["token cap".to_owned()]),
                expires_at: None,
                supersedes: Some("gotchas/older.md".to_owned()),
                salience: Some(2.5),
            })
            .expect("write");

        let page = server
            .read_page(ReadPageRequest {
                path: "gotchas/stale.md".to_owned(),
                global: None,
            })
            .expect("read");

        assert_eq!(page.status, "do-not-answer-from");
        assert_eq!(page.tier, "semantic");
        assert!(page.pinned);
        assert!(page.canonical);
        assert_eq!(page.entities, vec!["token cap".to_owned()]);
        assert_eq!(page.supersedes.as_deref(), Some("gotchas/older.md"));
        assert_eq!(page.salience, 2.5);
    }

    #[test]
    fn patch_preserves_metadata_and_clear_fields_changes_the_chain_explicitly() {
        let (_repo, _data, server) = harness();
        write_page(
            &server,
            "decisions/old.md",
            "Old decision",
            "The earlier answer.",
        );
        let created = server
            .write_page(WritePageRequest {
                path: "decisions/current.md".to_owned(),
                title: "Current decision".to_owned(),
                body: "The current answer.".to_owned(),
                tier: Some("semantic".to_owned()),
                status: Some("active".to_owned()),
                pinned: Some(true),
                canonical: Some(true),
                entities: Some(vec!["handoff".to_owned()]),
                expires_at: None,
                supersedes: Some("decisions/old.md".to_owned()),
                salience: Some(2.0),
            })
            .expect("create replacement");

        let patched = server
            .patch_page(PatchPageRequest {
                path: "decisions/current.md".to_owned(),
                expected_revision: created.revision,
                title: Some("Better current decision".to_owned()),
                body: Some("The corrected answer.".to_owned()),
                tier: None,
                status: None,
                pinned: None,
                canonical: None,
                entities: None,
                expires_at: None,
                supersedes: None,
                salience: None,
                page_abstract: None,
                clear_fields: None,
                global: None,
            })
            .expect("patch");
        for field in ["tier", "pinned", "canonical", "entities", "supersedes"] {
            assert!(
                patched.preserved.iter().any(|kept| kept == field),
                "{field}"
            );
        }
        let current = server
            .read_page(ReadPageRequest {
                path: "decisions/current.md".to_owned(),
                global: Some(false),
            })
            .unwrap();
        assert_eq!(current.tier, "semantic");
        assert!(current.pinned);
        assert!(current.canonical);
        assert_eq!(current.entities, vec!["handoff"]);
        assert_eq!(current.supersedes.as_deref(), Some("decisions/old.md"));
        let old = server
            .read_page(ReadPageRequest {
                path: "decisions/old.md".to_owned(),
                global: Some(false),
            })
            .unwrap();
        assert!(!old.is_latest);
        assert_eq!(old.status, "active", "authored status remains visible");
        assert_eq!(old.effective_status, "superseded");
        assert_eq!(old.superseded_by.as_deref(), Some("decisions/current.md"));

        let retired_patch = server
            .patch_page(PatchPageRequest {
                path: "decisions/old.md".to_owned(),
                expected_revision: old.revision,
                title: None,
                body: Some("Clarified old context.".to_owned()),
                tier: None,
                status: None,
                pinned: None,
                canonical: None,
                entities: None,
                expires_at: None,
                supersedes: None,
                salience: None,
                page_abstract: None,
                clear_fields: None,
                global: None,
            })
            .expect("patch retired page");
        assert!(!retired_patch.is_latest);
        assert_eq!(
            retired_patch.superseded_by.as_deref(),
            Some("decisions/current.md")
        );
        assert!(
            retired_patch
                .warnings
                .iter()
                .any(|warning| warning.contains("remains retired"))
        );

        let cleared = server
            .patch_page(PatchPageRequest {
                path: "decisions/current.md".to_owned(),
                expected_revision: patched.revision,
                title: None,
                body: None,
                tier: None,
                status: None,
                pinned: None,
                canonical: None,
                entities: None,
                expires_at: None,
                supersedes: None,
                salience: None,
                page_abstract: None,
                clear_fields: Some(vec!["supersedes".to_owned()]),
                global: None,
            })
            .expect("clear supersedes");
        assert_eq!(cleared.cleared, vec!["supersedes"]);
        assert!(
            cleared
                .chain_effects
                .iter()
                .any(|effect| { effect.path == "decisions/old.md" && effect.is_latest })
        );
        assert!(cleared.warnings.iter().any(|warning| {
            warning.contains("decisions/old.md") && warning.contains("chain head")
        }));
        assert!(
            server
                .read_page(ReadPageRequest {
                    path: "decisions/old.md".to_owned(),
                    global: Some(false),
                })
                .unwrap()
                .is_latest
        );
    }

    #[test]
    fn write_refuses_an_existing_path_and_names_the_safe_update_contract() {
        let (_repo, _data, server) = harness();
        write_page(&server, "notes/existing.md", "Existing", "original");
        let error = server
            .write_page(WritePageRequest {
                path: "notes/existing.md".to_owned(),
                title: "Replacement".to_owned(),
                body: "would overwrite".to_owned(),
                tier: None,
                status: None,
                pinned: None,
                canonical: None,
                entities: None,
                expires_at: None,
                supersedes: None,
                salience: None,
            })
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists at revision"), "{error}");
        assert!(error.contains("current frontmatter"), "{error}");
        assert!(error.contains("memory_patch_page"), "{error}");
    }

    /// A path out of a `memory_query` hit resolves whichever scope it came
    /// from: the caller is told which one, but is not required to say so.
    #[test]
    fn reading_falls_through_to_the_shared_scope() {
        let (_repo, _data, server) = harness();

        let global = server.global_scope();
        let page = anamnesis_wiki::page(
            global.project_id,
            PagePath::parse("_rules/style.md").expect("path"),
            Frontmatter::new("House style", Vec::new()).expect("frontmatter"),
            "Every project here wraps at eighty columns.",
        );
        server
            .wiki
            .lock()
            .write_page(&global.scope, &page, "test: shared page")
            .expect("write to the shared scope");

        let read = server
            .read_page(ReadPageRequest {
                path: "_rules/style.md".to_owned(),
                global: None,
            })
            .expect("the shared scope is searched after this project");

        assert!(read.global, "the caller is told where it came from");
        assert_eq!(read.title, "House style");
        assert!(read.body.contains("eighty columns"));
    }

    /// The project's own page wins a path both scopes hold: specificity is
    /// the reason the two scopes are separate at all.
    #[test]
    fn a_local_page_shadows_the_shared_one_at_the_same_path() {
        let (_repo, _data, server) = harness();

        let global = server.global_scope();
        let shared = anamnesis_wiki::page(
            global.project_id,
            PagePath::parse("_rules/style.md").expect("path"),
            Frontmatter::new("Shared style", Vec::new()).expect("frontmatter"),
            "The workspace-wide answer.",
        );
        server
            .wiki
            .lock()
            .write_page(&global.scope, &shared, "test: shared page")
            .expect("write to the shared scope");
        write_page(
            &server,
            "_rules/style.md",
            "Local style",
            "This project's answer.",
        );

        let read = server
            .read_page(ReadPageRequest {
                path: "_rules/style.md".to_owned(),
                global: None,
            })
            .expect("read");

        assert!(!read.global);
        assert_eq!(read.title, "Local style");
    }

    #[test]
    fn a_shared_query_hit_reads_the_shared_page_even_when_a_local_path_collides() {
        let (_repo, _data, server) = harness();
        let global = server.global_scope();
        let path = PagePath::parse("_rules/style.md").expect("path");
        let shared = anamnesis_wiki::page(
            global.project_id,
            path.clone(),
            Frontmatter::new("Shared style", Vec::new()).expect("frontmatter"),
            "Every project uses eighty columns.",
        );
        server
            .wiki
            .lock()
            .write_page(&global.scope, &shared, "test")
            .unwrap();
        server
            .store
            .upsert_project(&global, Timestamp::now())
            .unwrap();
        server
            .store
            .index_page(global.project_id, &shared, &[], None, Timestamp::now())
            .unwrap();
        write_page(
            &server,
            path.as_str(),
            "Local style",
            "This project uses tabs.",
        );

        let found = server
            .query(QueryRequest {
                text: "eighty columns".to_owned(),
                limit: None,
                explain: None,
            })
            .unwrap();
        let hit = found
            .hits
            .iter()
            .find(|hit| hit.global)
            .expect("shared hit");
        let before = server
            .store
            .sweep_row(global.project_id, &path)
            .unwrap()
            .unwrap();
        let local_before = server
            .store
            .sweep_row(server.scope.project_id, &path)
            .unwrap()
            .unwrap();
        let read = server
            .read_page(ReadPageRequest {
                path: hit.path.clone(),
                global: Some(hit.global),
            })
            .unwrap();
        assert!(read.global);
        assert!(read.body.contains("eighty columns"));
        assert_eq!(
            server
                .store
                .sweep_row(global.project_id, &path)
                .unwrap()
                .unwrap()
                .facts
                .access_count,
            before.facts.access_count + 1
        );
        assert_eq!(
            server
                .store
                .sweep_row(server.scope.project_id, &path)
                .unwrap()
                .unwrap()
                .facts
                .access_count,
            local_before.facts.access_count,
            "reading the shared page must not renew the local one"
        );

        let local = server
            .read_page(ReadPageRequest {
                path: path.to_string(),
                global: Some(false),
            })
            .unwrap();
        assert!(!local.global);
        assert!(local.body.contains("tabs"));
    }

    #[test]
    fn an_explicit_read_scope_never_substitutes_a_page_from_the_other_scope() {
        let (_repo, _data, server) = harness();
        write_page(&server, "notes/local.md", "Local", "Local content.");
        let global = server.global_scope();
        let shared = anamnesis_wiki::page(
            global.project_id,
            PagePath::parse("notes/shared.md").unwrap(),
            Frontmatter::new("Shared", Vec::new()).unwrap(),
            "Shared content.",
        );
        server
            .wiki
            .lock()
            .write_page(&global.scope, &shared, "test")
            .unwrap();
        for (path, from_global, scope) in [
            ("notes/local.md", true, "shared scope"),
            ("notes/shared.md", false, "this project"),
        ] {
            let error = server
                .read_page(ReadPageRequest {
                    path: path.to_owned(),
                    global: Some(from_global),
                })
                .unwrap_err()
                .to_string();
            assert!(error.contains(path) && error.contains(scope), "{error}");
        }
        let legacy: ReadPageRequest =
            serde_json::from_value(serde_json::json!({"path": "notes/shared.md"})).unwrap();
        assert!(server.read_page(legacy).unwrap().global);
    }

    /// Reading a page is using it, so it renews the page against the decay
    /// sweep exactly as a query that surfaced it would have.
    #[test]
    fn reading_a_page_renews_it() {
        let (_repo, _data, server) = harness();
        write_page(&server, "notes/a.md", "A", "content");
        let path = PagePath::parse("notes/a.md").expect("path");

        let before = server
            .store
            .sweep_row(server.scope.project_id, &path)
            .expect("sweep row")
            .expect("page is indexed");
        assert_eq!(before.facts.access_count, 0);

        server
            .read_page(ReadPageRequest {
                path: "notes/a.md".to_owned(),
                global: None,
            })
            .expect("read");

        let after = server
            .store
            .sweep_row(server.scope.project_id, &path)
            .expect("sweep row")
            .expect("page is indexed");
        assert_eq!(after.facts.access_count, 1);
        assert!(after.facts.last_accessed_at.is_some());
    }

    /// The default is silence: a caller that did not ask for the working does
    /// not get a field it has to learn to ignore.
    #[test]
    fn the_working_is_absent_unless_it_is_asked_for() {
        let (_repo, _data, server) = harness();
        write_page(&server, "notes/a.md", "A", "sqlite content");

        let quiet = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: None,
            })
            .expect("query");
        assert!(quiet.hits[0].explain.is_none());

        let loud = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");
        assert!(loud.hits[0].explain.is_some());
    }

    /// A stream that found the page reports a rank and a contribution; one
    /// that missed reports neither, and the miss is the more useful half —
    /// it is what a weight argument is made of.
    #[test]
    fn the_working_names_the_streams_that_found_the_page_and_the_ones_that_did_not() {
        let (_repo, _data, server) = harness();
        server
            .write_page(WritePageRequest {
                path: "decisions/0001-storage.md".to_owned(),
                title: "Storage engine".to_owned(),
                body: "We chose SQLite because the index is rebuildable.".to_owned(),
                tier: None,
                status: None,
                pinned: None,
                canonical: None,
                entities: Some(vec!["SQLite".to_owned()]),
                expires_at: None,
                supersedes: None,
                salience: None,
            })
            .expect("write");

        let found = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");
        let working = found.hits[0].explain.as_ref().expect("explain");

        assert_eq!(working.scope, "project");
        assert_eq!(working.fts.rank, Some(1), "full text found it first");
        assert!(working.fts.contribution > 0.0);
        assert_eq!(
            working.entity.rank,
            Some(1),
            "the page declares SQLite as an entity"
        );
        // No embedder is configured in this harness, so that stream is empty
        // for every query — and says so rather than reporting a rank of zero.
        assert_eq!(working.vectors.rank, None);
        assert_eq!(working.vectors.contribution, 0.0);
    }

    /// `fused` is the sum of what the streams contributed, and `within_scope`
    /// is that times the page's standing. Both are checkable from the same
    /// response, which is the point of printing them.
    #[test]
    fn the_working_adds_up() {
        let (_repo, _data, server) = harness();
        write_page(&server, "notes/a.md", "A", "sqlite content");

        let found = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");
        let working = found.hits[0].explain.as_ref().expect("explain");

        let summed = working.fts.contribution
            + working.entity.contribution
            + working.links.contribution
            + working.vectors.contribution;
        assert!((working.fused - summed).abs() < 1e-12, "{working:?}");
        assert!(
            (working.within_scope - working.fused).abs() < 1e-12,
            "{working:?}"
        );
        // `notes/` is not an authoritative namespace and the page is neither
        // pinned nor canonical, so there is nothing for standing to adjust.
        assert_eq!(working.standing, 1.0);
    }

    /// Standing moves a page up a ranking a stream already put it in, so an
    /// authoritative page reports one above 1.0 — and the explain is where
    /// that claim becomes checkable instead of documented.
    #[test]
    fn the_working_shows_standing_as_the_factor_it_is() {
        let (_repo, _data, server) = harness();
        server
            .write_page(WritePageRequest {
                path: "decisions/0001-storage.md".to_owned(),
                title: "Storage engine".to_owned(),
                body: "We chose SQLite.".to_owned(),
                tier: None,
                status: None,
                pinned: Some(true),
                canonical: Some(true),
                entities: None,
                expires_at: None,
                supersedes: None,
                salience: None,
            })
            .expect("write");

        let found = server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");
        let working = found.hits[0].explain.as_ref().expect("explain");

        assert!(
            working.standing > 1.0,
            "an authoritative, canonical, pinned page: {working:?}"
        );
        // The rank it was found at, counted from the place standing gave it.
        let rank = working.fts.rank.expect("full text found it") as f64;
        let expected = 1.0 / (Tuning::default().rrf_k + rank / working.standing);
        assert!(
            (working.fts.contribution - expected).abs() < 1e-12,
            "{working:?}"
        );
    }

    /// Asking which stream *would have* found a page is not the same as being
    /// handed the page, and the decay sweep reads those counters — so the
    /// explain pass must not inflate them.
    #[test]
    fn asking_for_the_working_does_not_count_as_a_second_read() {
        let (_repo, _data, server) = harness();
        write_page(&server, "notes/a.md", "A", "sqlite content");
        let path = PagePath::parse("notes/a.md").expect("path");

        server
            .query(QueryRequest {
                text: "sqlite".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");

        let row = server
            .store
            .sweep_row(server.scope.project_id, &path)
            .expect("sweep row")
            .expect("page is indexed");
        assert_eq!(
            row.facts.access_count, 1,
            "the search counts once; the explain pass adds nothing"
        );
    }

    /// A hit from the shared scope has its ranks read out of the shared
    /// scope's streams, not this project's — where it does not appear at all.
    #[test]
    fn the_working_reads_the_scope_the_hit_came_from() {
        let (_repo, _data, server) = harness();

        let global = server.global_scope();
        let page = anamnesis_wiki::page(
            global.project_id,
            PagePath::parse("_rules/style.md").expect("path"),
            Frontmatter::new("House style", Vec::new()).expect("frontmatter"),
            "Every project here wraps at eighty columns.",
        );
        server
            .wiki
            .lock()
            .write_page(&global.scope, &page, "test: shared page")
            .expect("write");
        server
            .store
            .upsert_project(&global, Timestamp::now())
            .expect("shared project row");
        server
            .store
            .index_page(global.project_id, &page, &[], None, Timestamp::now())
            .expect("index the shared page");

        let found = server
            .query(QueryRequest {
                text: "eighty columns".to_owned(),
                limit: None,
                explain: Some(true),
            })
            .expect("query");
        let hit = found
            .hits
            .iter()
            .find(|hit| hit.path == "_rules/style.md")
            .expect("the shared page is searched alongside this project");
        let working = hit.explain.as_ref().expect("explain");

        assert!(hit.global);
        assert_eq!(working.scope, "global");
        assert!(
            working.fts.rank.is_some(),
            "read out of the shared scope's streams, where it exists: {working:?}"
        );
    }

    /// The error says where it looked, because "not found" with one scope
    /// named and the other silent is the report that sends someone hunting
    /// for a typo that is not there.
    #[test]
    fn reading_a_missing_page_names_both_scopes() {
        let (_repo, _data, server) = harness();

        let error = server
            .read_page(ReadPageRequest {
                path: "notes/absent.md".to_owned(),
                global: None,
            })
            .expect_err("no such page");
        let message = error.to_string();

        assert!(message.contains("notes/absent.md"), "{message}");
        assert!(message.contains("shared scope"), "{message}");
    }
}
