//! HTTP surface: where hooks deliver events and starting sessions collect
//! their handoff.
//!
//! Hooks run inside someone's editing session. Whatever this server does, it
//! has to do it fast enough that nobody notices it is there — which is why the
//! capture path is a couple of SQLite inserts and nothing else, and why the
//! expensive work (summarising, writing markdown, committing) happens once, at
//! session end, rather than on every tool call.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anamnesis_core::session::AgentKind;
use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use jiff::Timestamp;
use parking_lot::Mutex;
use serde::Deserialize;

mod pipeline;

pub use pipeline::{Ingested, claim_handoff, finalize, ingest};

/// Errors surfaced over HTTP.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// The request was malformed or missing something required.
    #[error("{0}")]
    BadRequest(String),

    /// Storage failed.
    #[error("storage error: {0}")]
    Store(#[from] anamnesis_store::StoreError),

    /// The wiki failed.
    #[error("wiki error: {0}")]
    Wiki(#[from] anamnesis_wiki::WikiError),

    /// A core validation rejected the input.
    #[error("{0}")]
    Core(#[from] anamnesis_core::CoreError),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) | Self::Core(_) => StatusCode::BAD_REQUEST,
            Self::Store(_) | Self::Wiki(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // The message goes to the hook's stderr, where it is the only clue
        // anyone gets about why capture stopped working.
        (status, self.to_string()).into_response()
    }
}

/// Everything a handler needs.
#[derive(Clone)]
pub struct AppState {
    /// The SQLite index.
    pub store: Arc<Store>,
    /// The markdown wiki.
    ///
    /// Behind a mutex because a git repository is a single shared resource:
    /// the index file and HEAD are written during every commit, and two
    /// sessions ending at the same moment would otherwise race on both. One
    /// writer at a time is the same discipline the SQLite side follows.
    pub wiki: Arc<Mutex<Wiki>>,
}

impl AppState {
    /// Assemble state from an open index and wiki.
    pub fn new(store: Store, wiki: Wiki) -> Self {
        Self {
            store: Arc::new(store),
            wiki: Arc::new(Mutex::new(wiki)),
        }
    }
}

/// Build the router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/hook", post(receive_hook))
        .route("/handoff", get(deliver_handoff))
        .with_state(state)
}

/// Serve until the process ends.
pub async fn serve(bind: SocketAddr, state: AppState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "anamnesis listening");
    axum::serve(listener, router(state)).await
}

/// Liveness probe.
async fn health() -> &'static str {
    "ok\n"
}

/// Query string shared by both endpoints.
#[derive(Debug, Deserialize)]
struct AgentQuery {
    /// Which harness is calling. Payloads do not identify themselves.
    agent: Option<String>,
    /// Working directory, for the handoff endpoint where there is no body.
    cwd: Option<PathBuf>,
    /// The harness's session identifier.
    session_id: Option<String>,
}

impl AgentQuery {
    /// The harness, defaulting to Claude Code.
    fn agent(&self) -> AgentKind {
        self.agent
            .as_deref()
            .and_then(|value| value.parse().ok())
            .unwrap_or(AgentKind::ClaudeCode)
    }
}

/// Receive one lifecycle event.
///
/// Returns 202 rather than 200: the event has been accepted and made durable,
/// but whether it eventually becomes a wiki page is not something the caller
/// waits to find out.
async fn receive_hook(
    State(state): State<AppState>,
    Query(query): Query<AgentQuery>,
    body: String,
) -> Result<Response, WebError> {
    let payload: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| WebError::BadRequest(format!("payload is not JSON: {error}")))?;

    let hook = anamnesis_hooks::parse(&query.agent(), &payload)
        .map_err(|error| WebError::BadRequest(error.to_string()))?;

    if hook.was_redacted() {
        tracing::info!(rules = ?hook.redactions, "redacted secrets from hook payload");
    }

    let outcome = {
        let wiki = state.wiki.lock();
        ingest(&state.store, &wiki, &hook, Timestamp::now())?
    };
    if let Some(page) = &outcome.page {
        tracing::info!(%page, "session consolidated");
    }

    Ok((StatusCode::ACCEPTED, "accepted\n").into_response())
}

/// Hand a starting session its handoff.
///
/// The body is written straight to the hook's stdout, which the harness injects
/// into the model's context — so it is plain text, and empty when there is
/// nothing to say.
async fn deliver_handoff(
    State(state): State<AppState>,
    Query(query): Query<AgentQuery>,
) -> Result<String, WebError> {
    let cwd = query
        .cwd
        .clone()
        .ok_or_else(|| WebError::BadRequest("cwd is required".to_owned()))?;
    let session_id = query
        .session_id
        .clone()
        .ok_or_else(|| WebError::BadRequest("session_id is required".to_owned()))?;

    let handoff = claim_handoff(
        &state.store,
        &cwd,
        &query.agent(),
        &session_id,
        Timestamp::now(),
    )?;

    Ok(handoff.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::scope::resolve_scope;
    use serde_json::json;

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        state: AppState,
        cwd: PathBuf,
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

        Harness {
            cwd: repo.path().to_path_buf(),
            state: AppState::new(store, wiki),
            _repo: repo,
            _data: data,
        }
    }

    fn hook(harness: &Harness, event: &str, extra: serde_json::Value) -> anamnesis_hooks::ParsedHook {
        let mut payload = json!({
            "session_id": "session-abc",
            "hook_event_name": event,
            "cwd": harness.cwd.to_string_lossy(),
        });
        if let (Some(base), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        anamnesis_hooks::parse(&AgentKind::ClaudeCode, &payload).expect("parse")
    }

    fn now() -> Timestamp {
        Timestamp::now()
    }

    fn run(harness: &Harness, event: &str, extra: serde_json::Value) -> Ingested {
        ingest(
            &harness.state.store,
            &harness.state.wiki,
            &hook(harness, event, extra),
            now(),
        )
        .expect("ingest")
    }

    #[test]
    fn a_full_session_becomes_a_page_and_a_handoff() {
        let harness = harness();

        run(&harness, "SessionStart", json!({"source": "startup"}));
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "wire up the storage layer"}),
        );
        run(
            &harness,
            "PostToolUse",
            json!({
                "tool_name": "Edit",
                "tool_input": {"file_path": "crates/store/src/ops.rs"},
                "tool_response": {"success": true}
            }),
        );
        let end = run(&harness, "SessionEnd", json!({"reason": "clear"}));

        assert!(end.consolidated);
        let page = end.page.expect("a page was written");
        assert!(page.starts_with("sessions/"));

        // The page is on disk, in git, and in the index.
        let scope = resolve_scope(&harness.cwd).expect("scope");
        let path = anamnesis_core::page::PagePath::parse(&page).expect("path");
        let read = harness
            .state
            .wiki
            .read_page(&scope.scope, &path)
            .expect("page readable");
        assert!(read.body.contains("wire up the storage layer"));
        assert!(read.body.contains("crates/store/src/ops.rs"));
        assert_eq!(
            harness.state.store.page_count(scope.project_id).expect("count"),
            1
        );
        assert!(harness.state.wiki.commit_count().expect("commits") >= 1);
    }

    #[test]
    fn the_next_session_receives_the_handoff_once() {
        let harness = harness();
        run(&harness, "UserPromptSubmit", json!({"prompt": "do the thing"}));
        run(&harness, "SessionEnd", json!({}));

        let claim = || {
            claim_handoff(
                &harness.state.store,
                &harness.cwd,
                &AgentKind::ClaudeCode,
                "session-next",
                now(),
            )
            .expect("claim")
        };

        let first = claim().expect("a handoff was waiting");
        assert!(first.contains("do the thing"));
        assert_eq!(claim(), None, "the handoff is single use");
    }

    #[test]
    fn an_empty_session_leaves_nothing_behind() {
        let harness = harness();
        run(&harness, "SessionStart", json!({"source": "startup"}));
        let end = run(&harness, "SessionEnd", json!({}));

        assert!(end.consolidated);
        assert_eq!(end.page, None, "no page for a session with no work in it");

        let scope = resolve_scope(&harness.cwd).expect("scope");
        assert_eq!(
            harness.state.store.page_count(scope.project_id).expect("count"),
            0
        );
        assert_eq!(
            claim_handoff(
                &harness.state.store,
                &harness.cwd,
                &AgentKind::ClaudeCode,
                "session-next",
                now(),
            )
            .expect("claim"),
            None
        );
    }

    #[test]
    fn every_event_of_one_session_lands_on_one_row() {
        let harness = harness();
        let first = run(&harness, "SessionStart", json!({}));
        let second = run(&harness, "UserPromptSubmit", json!({"prompt": "hello"}));
        assert_eq!(first.session_id, second.session_id);

        let scope = resolve_scope(&harness.cwd).expect("scope");
        assert_eq!(
            harness
                .state
                .store
                .session_count(scope.project_id)
                .expect("count"),
            1
        );
    }

    #[test]
    fn secrets_never_reach_the_page() {
        let harness = harness();
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "deploy using AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENG"}),
        );
        let end = run(&harness, "SessionEnd", json!({}));

        let scope = resolve_scope(&harness.cwd).expect("scope");
        let path =
            anamnesis_core::page::PagePath::parse(&end.page.expect("page")).expect("path");
        let read = harness
            .state
            .wiki
            .read_page(&scope.scope, &path)
            .expect("read");
        assert!(!read.body.contains("wJalrXUtnFEMIK7MDENG"));
    }

    #[test]
    fn a_second_session_supersedes_an_unread_handoff() {
        let harness = harness();

        run(&harness, "UserPromptSubmit", json!({"prompt": "first task"}));
        run(&harness, "SessionEnd", json!({}));

        // A different agent session, ending without anyone having read the
        // first handoff.
        let mut payload = json!({
            "session_id": "session-two",
            "hook_event_name": "UserPromptSubmit",
            "cwd": harness.cwd.to_string_lossy(),
            "prompt": "second task"
        });
        let parse = |payload: &serde_json::Value| {
            anamnesis_hooks::parse(&AgentKind::ClaudeCode, payload).expect("parse")
        };
        ingest(
            &harness.state.store,
            &harness.state.wiki,
            &parse(&payload),
            now(),
        )
        .expect("ingest");
        payload["hook_event_name"] = json!("SessionEnd");
        ingest(
            &harness.state.store,
            &harness.state.wiki,
            &parse(&payload),
            now(),
        )
        .expect("ingest");

        let claimed = claim_handoff(
            &harness.state.store,
            &harness.cwd,
            &AgentKind::ClaudeCode,
            "session-three",
            now(),
        )
        .expect("claim")
        .expect("something pending");
        assert!(claimed.contains("second task"));
        assert!(!claimed.contains("first task"));
    }

    #[test]
    fn a_payload_without_a_cwd_is_refused() {
        let harness = harness();
        let payload = json!({"session_id": "s", "hook_event_name": "SessionStart"});
        let hook = anamnesis_hooks::parse(&AgentKind::ClaudeCode, &payload).expect("parse");
        let result = ingest(&harness.state.store, &harness.state.wiki, &hook, now());
        assert!(matches!(result, Err(WebError::BadRequest(_))));
    }
}
