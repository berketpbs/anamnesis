//! A JSON API for reading memory, under `/api/v1`.
//!
//! The browser at `/ui` renders the same facts for a person; this serves them
//! to a program. Both exist because the rest of the HTTP surface answers only
//! two questions — take this event, hand me my handoff — and neither of them
//! is "what does memory hold", which is what anything built around anamnesis
//! needs to ask: a dashboard, a script that exports a project's pages, another
//! agent that is not speaking MCP.
//!
//! **Read-only, deliberately.** Every write in this system is either capture
//! (a hook, which has its own endpoint) or a decision somebody made (a page
//! written, a page forgotten, a proposal applied) — and those are CLI commands
//! on purpose, so that changing memory takes a machine somebody has rather
//! than a token somebody has. The audit log this API exposes is the record of
//! exactly those decisions, and it would be a strange thing to publish through
//! a door that could make them.
//!
//! **Versioned from the first line.** `/api/v1` is a promise that a consumer
//! written today keeps working; anything that would break it becomes `/api/v2`
//! beside it rather than a surprise on somebody's dashboard.
//!
//! Everything here goes through [`crate::off_runtime`] for the reason
//! everything else does: these are SQLite queries and file reads, and a
//! listing of a large wiki must not hold the thread `/health` is answered on.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use anamnesis_core::page::PagePath;
use anamnesis_store::ProjectRow;
use jiff::Timestamp;

use crate::{AppState, off_runtime};

/// Prefix every route in this module sits under.
pub(crate) const PREFIX: &str = "/api/v1";

/// How many hits a search returns when nobody says.
const SEARCH_LIMIT: usize = 20;

/// How many rows a listing returns when nobody says.
const PAGE_LIMIT: usize = 100;

/// Routes for the JSON API, behind the same header-only guard the rest of the
/// API uses.
///
/// Not the browser's guard: a credential a browser attaches on its own must
/// not be able to read the whole of memory from a page on another site.
pub(crate) fn routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route(&format!("{PREFIX}/scopes"), get(scopes))
        .route(
            &format!("{PREFIX}/scopes/{{workspace}}/{{project}}/pages"),
            get(pages),
        )
        .route(
            &format!("{PREFIX}/scopes/{{workspace}}/{{project}}/pages/{{*path}}"),
            get(page),
        )
        .route(
            &format!("{PREFIX}/scopes/{{workspace}}/{{project}}/search"),
            get(search),
        )
        .route(
            &format!("{PREFIX}/scopes/{{workspace}}/{{project}}/sessions"),
            get(sessions),
        )
        .route(
            &format!("{PREFIX}/scopes/{{workspace}}/{{project}}/audit"),
            get(audit),
        )
        // `route_layer`, so a path this server does not serve is a 404 rather
        // than a 401 that implies it exists.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::require_token,
        ))
}

/// What went wrong, as JSON.
///
/// A program consuming this should not have to parse an HTML error page to
/// find out that a scope does not exist.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    /// No such scope, or no such page in it.
    #[error("{0}")]
    Missing(String),

    /// The request named something that is not a page path.
    #[error("{0}")]
    Invalid(String),

    /// The index could not be read.
    #[error("storage error: {0}")]
    Store(#[from] anamnesis_store::StoreError),

    /// The wiki could not be read.
    #[error("wiki error: {0}")]
    Wiki(#[from] anamnesis_wiki::WikiError),

    /// Reading panicked on the blocking pool, where it runs.
    #[error("the server failed while reading this: {0}")]
    Panicked(String),
}

impl From<anamnesis_core::CoreError> for ApiError {
    fn from(error: anamnesis_core::CoreError) -> Self {
        Self::Invalid(error.to_string())
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Panicked(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Missing(_) => StatusCode::NOT_FOUND,
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Store(_) | Self::Wiki(_) | Self::Panicked(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ErrorBody {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

/// The body every failure answers with.
#[derive(Serialize)]
struct ErrorBody {
    /// What went wrong, in a sentence.
    error: String,
}

/// One scope this server holds memory for.
#[derive(Serialize)]
struct ScopeSummary {
    workspace: String,
    project: String,
    pages: i64,
    sessions: i64,
    /// When capture last reached this project, if it ever has.
    last_event_at: Option<String>,
}

/// A page, as it appears in a listing.
#[derive(Serialize)]
struct PageSummary {
    path: String,
    title: String,
    tier: String,
    status: String,
    pinned: bool,
    canonical: bool,
    written_at: String,
    reads: u32,
}

/// A page, with what it says.
#[derive(Serialize)]
struct PageDetail {
    path: String,
    title: String,
    tier: String,
    status: String,
    pinned: bool,
    canonical: bool,
    /// Set when something has replaced this page, because retrieval stopped
    /// offering it the moment that happened and a reader who arrived by name
    /// has no other way to find out.
    superseded_by: Option<String>,
    /// The page as the wiki holds it, not as the index copied it: the file is
    /// what a person edits and what git keeps.
    body: String,
}

/// One search hit.
#[derive(Serialize)]
struct Hit {
    path: String,
    title: String,
    tier: String,
    status: String,
    score: f64,
    /// The leading slice of the body, for a caller deciding whether to read
    /// the whole page.
    snippet: String,
    /// True when the hit came from the workspace's shared scope rather than
    /// this project — a different kind of answer, and the path does not say.
    shared: bool,
}

/// One recorded session.
#[derive(Serialize)]
struct SessionRow {
    id: String,
    agent: String,
    state: String,
    started_at: String,
    ended_at: Option<String>,
    workstream: Option<String>,
    operator: Option<String>,
    observations: i64,
}

/// One deliberate change to memory.
#[derive(Serialize)]
struct AuditRow {
    at: String,
    operator: Option<String>,
    via: String,
    action: String,
    subject: String,
    detail: Option<String>,
}

/// How many rows to return.
#[derive(Debug, Deserialize)]
struct Limit {
    limit: Option<usize>,
}

/// A question asked of a scope.
#[derive(Debug, Deserialize)]
struct Ask {
    q: Option<String>,
    limit: Option<usize>,
}

/// Every scope this server holds memory for.
async fn scopes(State(state): State<AppState>) -> Result<Json<Vec<ScopeSummary>>, ApiError> {
    off_runtime(move || {
        let mut out = Vec::new();
        for row in state.store.projects()? {
            out.push(ScopeSummary {
                workspace: row.scope.workspace.to_string(),
                project: row.scope.project.to_string(),
                pages: state.store.page_count(row.project_id)?,
                sessions: state.store.session_count(row.project_id)?,
                last_event_at: state
                    .store
                    .last_observation_at(row.project_id)?
                    .map(|at| at.to_string()),
            });
        }
        Ok(Json(out))
    })
    .await
}

/// One scope's pages, newest first.
async fn pages(
    State(state): State<AppState>,
    Path((workspace, project)): Path<(String, String)>,
    Query(limit): Query<Limit>,
) -> Result<Json<Vec<PageSummary>>, ApiError> {
    off_runtime(move || {
        let found = find_scope(&state, &workspace, &project)?;
        let mut rows = state.store.sweep_rows(found.project_id)?;
        // The same order the browser lists in: a memory is read to see what it
        // has learned lately far more often than alphabetically.
        rows.sort_by(|a, b| {
            b.facts
                .written_at
                .cmp(&a.facts.written_at)
                .then_with(|| a.path.as_str().cmp(b.path.as_str()))
        });
        Ok(Json(
            rows.into_iter()
                .take(limit.limit.unwrap_or(PAGE_LIMIT))
                .map(|row| PageSummary {
                    path: row.path.to_string(),
                    title: row.title,
                    tier: row.facts.tier.as_str().to_owned(),
                    status: row.facts.status.as_str().to_owned(),
                    pinned: row.facts.pinned,
                    canonical: row.facts.canonical,
                    written_at: row.facts.written_at.to_string(),
                    reads: row.facts.access_count,
                })
                .collect(),
        ))
    })
    .await
}

/// One page, read from the wiki.
async fn page(
    State(state): State<AppState>,
    Path((workspace, project, path)): Path<(String, String, String)>,
) -> Result<Json<PageDetail>, ApiError> {
    off_runtime(move || {
        let found = find_scope(&state, &workspace, &project)?;
        let page_path = PagePath::parse(&path)?;

        let parsed = {
            let wiki = state.wiki.lock();
            if !wiki.exists(&found.scope, &page_path) {
                return Err(ApiError::Missing(format!(
                    "no page at {path} in {workspace}/{project}"
                )));
            }
            wiki.read_page(&found.scope, &page_path)?
        };

        let superseded = state
            .store
            .superseded_by(found.project_id, &page_path)?
            .map(|path| path.to_string());

        Ok(Json(PageDetail {
            path: page_path.to_string(),
            title: parsed.frontmatter.title.clone(),
            tier: parsed.frontmatter.tier.as_str().to_owned(),
            status: parsed.frontmatter.status.as_str().to_owned(),
            pinned: parsed.frontmatter.pinned,
            canonical: parsed.frontmatter.canonical,
            superseded_by: superseded,
            body: parsed.body,
        }))
    })
    .await
}

/// The same fused query an agent runs, asked of one scope.
async fn search(
    State(state): State<AppState>,
    Path((workspace, project)): Path<(String, String)>,
    Query(ask): Query<Ask>,
) -> Result<Json<Vec<Hit>>, ApiError> {
    off_runtime(move || {
        let found = find_scope(&state, &workspace, &project)?;
        let query = ask.q.unwrap_or_default();
        let asked = query.trim();
        if asked.is_empty() {
            return Err(ApiError::Invalid("q is required".to_owned()));
        }

        // A broken or slow embedder costs the search its fourth stream, not
        // the search — the same rule an agent's query follows.
        let vector = state.embedder.as_ref().and_then(|embedder| {
            match embedder.embed(asked) {
                Ok(vector) => Some((embedder.model().to_owned(), vector)),
                Err(error) => {
                    tracing::warn!(%error, "query embedding failed; searching without the vector stream");
                    None
                }
            }
        });

        let global = state.wiki.lock().global_scope(&found.scope.workspace);
        let hits = state.store.query_pages_across(
            found.project_id,
            &[global.project_id],
            asked,
            ask.limit.unwrap_or(SEARCH_LIMIT),
            Timestamp::now(),
            vector
                .as_ref()
                .map(|(model, vector)| (model.as_str(), vector.as_slice())),
        )?;

        Ok(Json(
            hits.into_iter()
                .map(|hit| Hit {
                    shared: hit.project_id != found.project_id,
                    path: hit.path.to_string(),
                    title: hit.title,
                    tier: hit.tier.as_str().to_owned(),
                    status: hit.status.as_str().to_owned(),
                    score: hit.score,
                    snippet: hit.snippet,
                })
                .collect(),
        ))
    })
    .await
}

/// One scope's sessions, newest first.
async fn sessions(
    State(state): State<AppState>,
    Path((workspace, project)): Path<(String, String)>,
    Query(limit): Query<Limit>,
) -> Result<Json<Vec<SessionRow>>, ApiError> {
    off_runtime(move || {
        let found = find_scope(&state, &workspace, &project)?;
        let rows = state
            .store
            .recent_sessions(found.project_id, limit.limit.unwrap_or(PAGE_LIMIT))?;
        Ok(Json(
            rows.into_iter()
                .map(|row| SessionRow {
                    id: row.id.to_string(),
                    agent: row.agent,
                    state: row.state,
                    started_at: row.started_at.to_string(),
                    ended_at: row.ended_at.map(|at| at.to_string()),
                    workstream: row.workstream,
                    operator: row.operator,
                    observations: row.observation_count,
                })
                .collect(),
        ))
    })
    .await
}

/// What has been changed by hand, newest first.
async fn audit(
    State(state): State<AppState>,
    Path((workspace, project)): Path<(String, String)>,
    Query(limit): Query<Limit>,
) -> Result<Json<Vec<AuditRow>>, ApiError> {
    off_runtime(move || {
        let found = find_scope(&state, &workspace, &project)?;
        let trail = state
            .store
            .audit_trail(Some(found.project_id), limit.limit.unwrap_or(PAGE_LIMIT))?;
        Ok(Json(
            trail
                .into_iter()
                .map(|entry| AuditRow {
                    at: entry.at.to_string(),
                    operator: entry.operator.map(|name| name.to_string()),
                    via: entry.via.as_str().to_owned(),
                    action: entry.action.as_str().to_owned(),
                    subject: entry.subject,
                    detail: entry.detail,
                })
                .collect(),
        ))
    })
    .await
}

/// The scope a request names, or a 404 that says which one was meant.
fn find_scope(state: &AppState, workspace: &str, project: &str) -> Result<ProjectRow, ApiError> {
    state
        .store
        .projects()?
        .into_iter()
        .find(|row| {
            row.scope.workspace.as_str() == workspace && row.scope.project.as_str() == project
        })
        .ok_or_else(|| ApiError::Missing(format!("no scope named {workspace}/{project} here")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use anamnesis_core::page::{Frontmatter, Page, PageStatus, Tier};
    use anamnesis_core::scope::{ResolvedScope, resolve_scope};
    use anamnesis_store::Store;
    use anamnesis_wiki::Wiki;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        state: AppState,
        scope: ResolvedScope,
    }

    fn harness() -> Harness {
        let repo = tempfile::tempdir().expect("repo dir");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");

        let data = tempfile::tempdir().expect("data dir");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");

        let scope = resolve_scope(repo.path()).expect("scope");
        store.upsert_project(&scope, now()).expect("project");

        Harness {
            state: AppState::new(store, wiki),
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-09-01T09:00:00Z".parse().expect("timestamp")
    }

    /// Write a page the way every real writer does: the wiki first, then the
    /// index. The API reads both, and a test that only touched one would pass
    /// on a page no reader could actually open.
    fn write(harness: &Harness, path: &str, title: &str, body: &str) {
        let mut frontmatter = Frontmatter::new(title, Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Semantic;
        frontmatter.status = PageStatus::Active;
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            frontmatter,
            body,
        );
        harness
            .state
            .wiki
            .lock()
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
        harness
            .state
            .store
            .upsert_page(&page, now())
            .expect("index");
    }

    async fn get(state: &AppState, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = crate::router(state.clone(), false)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn the_scope_listing_says_what_each_project_holds() {
        let harness = harness();
        write(&harness, "notes/api.md", "The API", "what we decided");

        let (status, body) = get(&harness.state, "/api/v1/scopes").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[0]["workspace"], "default");
        assert_eq!(body[0]["project"], "widget");
        assert_eq!(body[0]["pages"], 1);
    }

    #[tokio::test]
    async fn a_page_comes_back_with_the_body_the_wiki_holds() {
        let harness = harness();
        write(&harness, "notes/api.md", "The API", "what we decided");

        let (status, body) = get(
            &harness.state,
            "/api/v1/scopes/default/widget/pages/notes/api.md",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["path"], "notes/api.md");
        assert_eq!(body["title"], "The API");
        assert_eq!(body["tier"], "semantic");
        assert!(
            body["body"]
                .as_str()
                .expect("body")
                .contains("what we decided"),
            "{body}"
        );
    }

    /// The listing and the page are different queries against different
    /// stores, and a page in one and not the other is the drift the browser
    /// already reports. Both have to name the same page.
    #[tokio::test]
    async fn the_listing_names_the_pages_that_can_be_read() {
        let harness = harness();
        write(&harness, "notes/api.md", "The API", "one");
        write(&harness, "notes/decisions.md", "Decisions", "two");

        let (status, body) = get(&harness.state, "/api/v1/scopes/default/widget/pages").await;

        assert_eq!(status, StatusCode::OK);
        let paths: Vec<&str> = body
            .as_array()
            .expect("array")
            .iter()
            .map(|row| row["path"].as_str().expect("path"))
            .collect();
        assert_eq!(paths.len(), 2, "{body}");
        assert!(paths.contains(&"notes/api.md"), "{body}");
        assert!(paths.contains(&"notes/decisions.md"), "{body}");
    }

    #[tokio::test]
    async fn a_search_answers_with_the_pages_it_found() {
        let harness = harness();
        write(
            &harness,
            "notes/api.md",
            "The API",
            "the retry budget is two seconds",
        );
        write(&harness, "notes/other.md", "Other", "unrelated prose");

        let (status, body) = get(
            &harness.state,
            "/api/v1/scopes/default/widget/search?q=retry%20budget",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[0]["path"], "notes/api.md", "{body}");
        assert!(body[0]["score"].as_f64().expect("score") > 0.0, "{body}");
    }

    /// A question with no question in it is the caller's mistake, and saying
    /// so is cheaper for them than an empty array they have to interpret.
    #[tokio::test]
    async fn a_search_without_a_question_says_so() {
        let harness = harness();

        let (status, body) = get(&harness.state, "/api/v1/scopes/default/widget/search").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .expect("error")
                .contains("q is required"),
            "{body}"
        );
    }

    /// Errors are JSON too. A program consuming this should not have to parse
    /// an HTML page to find out that a scope does not exist.
    #[tokio::test]
    async fn a_scope_that_is_not_here_is_a_json_404() {
        let harness = harness();

        let (status, body) = get(&harness.state, "/api/v1/scopes/default/nowhere/pages").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]
                .as_str()
                .expect("error")
                .contains("no scope named default/nowhere"),
            "{body}"
        );
    }

    /// The audit log is the whole reason a shared server is trustworthy, and
    /// reading it is the one thing a dashboard most needs.
    #[tokio::test]
    async fn the_audit_trail_is_readable_through_the_api() {
        let harness = harness();
        harness
            .state
            .store
            .append_audit(
                &anamnesis_core::audit::AuditEntry::new(
                    anamnesis_core::audit::Action::PageForgotten,
                    anamnesis_core::audit::Via::Cli,
                    "notes/gone.md",
                    now(),
                )
                .in_project(harness.scope.project_id),
            )
            .expect("append");

        let (status, body) = get(&harness.state, "/api/v1/scopes/default/widget/audit").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[0]["action"], "page.forgotten");
        assert_eq!(body[0]["subject"], "notes/gone.md");
        assert_eq!(body[0]["via"], "cli");
    }

    /// The guard is the API's, not the browser's: a credential a browser
    /// attaches on its own must not read the whole of memory from a page on
    /// somebody else's site.
    #[tokio::test]
    async fn a_guarded_server_refuses_an_unauthenticated_read() {
        let harness = harness();
        let guarded = harness
            .state
            .clone()
            .with_auth(crate::Auth::parse(None, Some("alice=swordfish")).expect("tokens"));

        let (status, _) = get(&guarded, "/api/v1/scopes").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let response = crate::router(guarded, false)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/scopes")
                    .header("authorization", "Bearer swordfish")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
