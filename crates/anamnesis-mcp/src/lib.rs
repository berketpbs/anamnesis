//! MCP server implementation for anamnesis.
//!
//! Exposes three tools over the Model Context Protocol: `memory_query`,
//! `memory_write_page`, and `memory_handoff_accept`. All three operate against
//! one resolved scope — the project the server was started against — the same
//! way `anamnesis serve` binds to one project's store and wiki rather than
//! discovering scope per request.
//!
//! Transport is the caller's choice (stdio is what `anamnesis mcp` uses); this
//! crate only implements [`ServerHandler`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::SessionId;
use anamnesis_core::page::{Entity, Frontmatter, Page, PagePath, PageStatus, Tier};
use anamnesis_core::scope::{OperatorName, ResolvedScope};
use anamnesis_core::session::AgentKind;
use anamnesis_core::workstream::{Workstream, WorkstreamSlug};
use anamnesis_llm::Embedder;
use anamnesis_store::{Store, new_session};
use anamnesis_wiki::Wiki;
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
}

/// Response for [`AnamnesisMcp::memory_query`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryResponse {
    /// Matching pages, best match first.
    pub hits: Vec<QueryHit>,
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
        description = "Search the project's memory wiki for pages relevant to a query."
    )]
    pub async fn memory_query(
        &self,
        params: Parameters<QueryRequest>,
    ) -> Result<Json<QueryResponse>, String> {
        self.query(params.0)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    /// Write or update a wiki page.
    ///
    /// Writing the same path again replaces its content and adds a new git
    /// commit; the page's identity (and therefore its decay history) does not
    /// change, because page identifiers are derived from `(project, path)`.
    #[tool(
        name = "memory_write_page",
        description = "Write or update a page in the project's memory wiki."
    )]
    pub async fn memory_write_page(
        &self,
        params: Parameters<WritePageRequest>,
    ) -> Result<Json<WritePageResponse>, String> {
        self.write_page(params.0)
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
        description = "Claim the pending handoff left by the previous session, if any."
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
        description = "Start or resume a named workstream: a persistent thread of work spanning many sessions and harnesses."
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
        description = "Show a workstream's status and its event ledger (sessions and handoffs)."
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
             might already have prior decisions, gotchas, or context recorded. Call \
             memory_write_page to record durable knowledge — decisions, gotchas, procedures — \
             worth keeping past this session; ordinary session summaries are written \
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
        let hits = self.store.query_pages_across(
            self.scope.project_id,
            &[global.project_id],
            &request.text,
            limit,
            Timestamp::now(),
            query_vector
                .as_ref()
                .map(|(model, vector)| (model.as_str(), vector.as_slice())),
        )?;
        Ok(QueryResponse {
            hits: hits
                .into_iter()
                .map(|hit| QueryHit {
                    path: hit.path.as_str().to_owned(),
                    title: hit.title,
                    tier: hit.tier.as_str().to_owned(),
                    status: hit.status.as_str().to_owned(),
                    pinned: hit.pinned,
                    canonical: hit.canonical,
                    score: hit.score,
                    snippet: hit.snippet,
                    global: hit.project_id == global.project_id,
                })
                .collect(),
        })
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
        let commit = {
            let wiki = self.wiki.lock();
            wiki.write_page(&self.scope.scope, &page, &format!("mcp: write {path}"))?
        };
        page.git_commit = Some(commit.clone());

        let now = Timestamp::now();
        self.store.upsert_project(&self.scope, now)?;
        self.store.upsert_page(&page, now)?;
        self.store
            .set_page_entities(self.scope.project_id, page.id, &entities)?;
        let links = anamnesis_wiki::extract_links(&request.body);
        self.store
            .set_page_links(self.scope.project_id, page.id, &links)?;

        // Embedding failure costs this page its place in the vector stream,
        // not the write itself — the page is already committed to the wiki
        // and indexed by the time this runs.
        if let Some(embedder) = &self.embedder {
            let text = format!("{}\n\n{}", request.title, request.body);
            match embedder.embed(&text) {
                Ok(vector) => {
                    self.store
                        .set_page_embedding(page.id, embedder.model(), &vector)?;
                }
                Err(error) => {
                    tracing::warn!(%error, %path, "page embedding failed; page was still written");
                }
            }
        }

        Ok(WritePageResponse {
            path: path.as_str().to_owned(),
            commit,
        })
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
        let root = {
            let wiki = self.wiki.lock();
            wiki.root()
                .join(self.scope.scope.workspace.as_str())
                .join(anamnesis_core::scope::GLOBAL_PROJECT)
        };
        ResolvedScope::global(&self.scope.scope.workspace, root)
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
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert!(names.contains(&"memory_query"));
        assert!(names.contains(&"memory_write_page"));
        assert!(names.contains(&"memory_handoff_accept"));
        assert!(names.contains(&"workstream_start"));
        assert!(names.contains(&"workstream_status"));
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
        fn model(&self) -> &str {
            "fake-embed-1"
        }
        fn dimension(&self) -> usize {
            2
        }
        fn embed(&self, _text: &str) -> Result<Vec<f32>, anamnesis_llm::EmbedError> {
            Ok(vec![1.0, 0.0])
        }
    }

    struct BrokenEmbedder;

    impl Embedder for BrokenEmbedder {
        fn model(&self) -> &str {
            "broken"
        }
        fn dimension(&self) -> usize {
            2
        }
        fn embed(&self, _text: &str) -> Result<Vec<f32>, anamnesis_llm::EmbedError> {
            Err(anamnesis_llm::EmbedError::Inference("boom".to_owned()))
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
            })
            .expect("query should still succeed via the other streams");
        assert_eq!(found.hits.len(), 1);
    }
}
