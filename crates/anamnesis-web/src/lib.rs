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

use anamnesis_core::observation::EventKind;
use anamnesis_core::session::AgentKind;
use anamnesis_llm::Provider;
use anamnesis_store::{RawSpool, Store};
use anamnesis_wiki::Wiki;
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use jiff::Timestamp;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod improve;
mod pipeline;
pub mod watch;

pub use auth::{Auth, Identity};
pub use pipeline::{Ingested, claim_handoff, finalize, finalize_with_llm, ingest, record};

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

/// The model to consolidate with, and the budgets it works inside.
#[derive(Clone)]
pub struct LlmSettings {
    /// The provider to ask.
    pub provider: Arc<dyn Provider>,
    /// Prompt budget, in estimated tokens.
    pub max_input_tokens: usize,
    /// Reply budget, in tokens.
    pub max_output_tokens: u32,
}

impl std::fmt::Debug for LlmSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmSettings")
            .field("provider", &self.provider.name())
            .field("model", &self.provider.model())
            .finish_non_exhaustive()
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
    /// The durable transcript every captured observation is also written to.
    ///
    /// `None` disables spooling, which is what tests that only care about
    /// the index use; a server started by the CLI always has one.
    pub raw: Option<Arc<RawSpool>>,
    /// The model, when one is configured. `None` means every session is
    /// summarised by counting.
    pub llm: Option<LlmSettings>,
    /// The tokens this server accepts. Open by default, which is what every
    /// install did before tokens existed.
    pub auth: Auth,
}

impl AppState {
    /// Assemble state from an open index and wiki, with no model.
    pub fn new(store: Store, wiki: Wiki) -> Self {
        Self {
            store: Arc::new(store),
            wiki: Arc::new(Mutex::new(wiki)),
            raw: None,
            llm: None,
            auth: Auth::open(),
        }
    }

    /// Spool every observation to a durable transcript as well as the index.
    pub fn with_raw(mut self, raw: Option<RawSpool>) -> Self {
        self.raw = raw.map(Arc::new);
        self
    }

    /// Consolidate with a model, when one was configured.
    pub fn with_llm(mut self, settings: Option<LlmSettings>) -> Self {
        self.llm = settings;
        self
    }

    /// Require one of these tokens on every request that touches memory.
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }
}

/// Build the router.
///
/// `/health` is outside the guard on purpose. It says only that an anamnesis
/// server is listening — which the open port already says — and keeping it
/// answerable is what lets `anamnesis status` tell "the server is down" apart
/// from "the server is up and does not accept your token". Collapsing those two
/// into one silence is how a person spends an afternoon restarting a server
/// that was running the whole time.
pub fn router(state: AppState) -> Router {
    let guarded = Router::new()
        .route("/hook", post(receive_hook))
        .route("/handoff", get(deliver_handoff))
        .route("/whoami", get(whoami))
        // `route_layer`, not `layer`: a request for a path this server does not
        // serve should be a 404, not a 401 that implies the path exists.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ));

    Router::new()
        .route("/health", get(health))
        .merge(guarded)
        .with_state(state)
}

/// Turn away requests that do not carry an accepted token.
///
/// On an open server this resolves to [`Identity::Anonymous`] and costs a
/// header lookup. The identity is put into the request's extensions either way,
/// so a handler downstream never has to ask again — or care which of the two
/// ways it got there.
async fn require_token(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    match state.auth.authenticate(header) {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(rejection) => {
            // Logged at warn: on a server that requires tokens, a rejected
            // request is either a misconfigured hook that has stopped
            // recording anything or somebody else knocking. Both are worth a
            // line, and neither is worth the token that was presented.
            tracing::warn!(
                path = %request.uri().path(),
                reason = ?rejection,
                "rejected an unauthenticated request"
            );
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                format!("{}\n", rejection.message()),
            )
                .into_response()
        }
    }
}

/// What the server makes of the caller's token.
#[derive(Debug, Serialize)]
struct WhoAmI {
    /// `open` when no token is required, `token` when one was accepted.
    auth: &'static str,
    /// The operator the token belongs to, when it names one.
    operator: Option<String>,
}

/// Report the caller's identity back to them.
///
/// The endpoint exists for the question `status` has to answer — "does this
/// machine's token work?" — which no other route can answer without side
/// effects: `/handoff` consumes a handoff, and `/hook` records an event.
async fn whoami(Extension(identity): Extension<Identity>) -> Json<WhoAmI> {
    Json(WhoAmI {
        auth: if identity.is_anonymous() {
            "open"
        } else {
            "token"
        },
        operator: identity.operator().map(ToString::to_string),
    })
}

/// Serve until the process ends.
pub async fn serve(bind: SocketAddr, state: AppState, watch_wiki: bool) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "anamnesis listening");

    // The server is the only part of the system that runs for longer than one
    // command, so it is where a schedule can live. It costs nothing until a
    // project turns one on: every tick over a fleet that has not asked for
    // auto-improve is one query and a list of reasons why not.
    tokio::spawn(improve::run_scheduler(state.clone()));

    // On by default, unlike the scheduler, and the difference is what each one
    // does: auto-improve makes decisions about someone's memory, so it waits to
    // be asked. The watcher only makes the index say what the wiki already
    // says. A blocking task because everything it touches — SQLite, git, the
    // wiki mutex — is synchronous.
    if watch_wiki {
        let watching = state.clone();
        tokio::task::spawn_blocking(move || watch::run(watching));
    }

    axum::serve(listener, router(state)).await
}

/// Liveness probe.
async fn health() -> &'static str {
    "ok\n"
}

/// Read a hook body as JSON.
///
/// Windows shells prepend a UTF-8 byte order mark when piping text into a
/// native process, and a BOM is not valid JSON. Tolerating it here is what lets
/// the same hook command work from PowerShell, cmd, and a POSIX shell alike —
/// and the failure it prevents is an invisible one, because a rejected event
/// looks exactly like a session where nothing happened.
fn parse_payload(body: &str) -> Result<serde_json::Value, WebError> {
    let trimmed = body.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(WebError::BadRequest("payload was empty".to_owned()));
    }
    serde_json::from_str(trimmed)
        .map_err(|error| WebError::BadRequest(format!("payload is not JSON: {error}")))
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
    let payload = parse_payload(&body)?;

    let hook = anamnesis_hooks::parse(&query.agent(), &payload)
        .map_err(|error| WebError::BadRequest(error.to_string()))?;

    if hook.was_redacted() {
        tracing::info!(rules = ?hook.redactions, "redacted secrets from hook payload");
    }

    let now = Timestamp::now();

    let Some(settings) = state.llm.clone() else {
        // No model: summarising is counting, which is fast enough to finish
        // while the hook waits.
        let outcome = {
            let wiki = state.wiki.lock();
            ingest(&state.store, &wiki, state.raw.as_deref(), &hook, now)?
        };
        if let Some(page) = &outcome.page {
            tracing::info!(%page, "session consolidated");
        }
        return Ok((StatusCode::ACCEPTED, "accepted\n").into_response());
    };

    // With a model the two halves come apart. Recording is synchronous and
    // its failure is the caller's business — a rejected event is one the hook
    // should complain about. Consolidation is not: it takes seconds, and the
    // hook waiting for it is a subprocess of somebody's editor that gives up
    // after one. So the response goes out now and the page is written behind
    // it. The cost of that choice is honest: a server killed in the next few
    // seconds loses the page, and the session stays open rather than closing
    // with nothing in it.
    let (scope, session_id) = record(&state.store, state.raw.as_deref(), &hook, now)?;

    if hook.kind == EventKind::SessionEnd {
        let background = state.clone();
        tokio::spawn(async move {
            let outcome = finalize_with_llm(
                &background.store,
                &background.wiki,
                &scope,
                session_id,
                now,
                &settings,
            )
            .await;

            match outcome {
                Ok(Some(page)) => tracing::info!(%page, "session consolidated"),
                Ok(None) => {}
                // There is nothing left to report this to — the hook exited
                // long ago — so this log line is the only record that a
                // session ended without leaving a page.
                Err(error) => tracing::error!(%error, %session_id, "consolidation failed"),
            }
        });
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
        harness_with("")
    }

    /// A harness whose marker file carries `extra` beyond the scope table.
    fn harness_with(extra: &str) -> Harness {
        let repo = tempfile::tempdir().expect("repo dir");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            format!("[scope]\nworkspace = \"default\"\nproject = \"widget\"\n{extra}"),
        )
        .expect("marker");

        let data = tempfile::tempdir().expect("data dir");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");
        let raw = RawSpool::new(data.path().join("raw"));

        Harness {
            cwd: repo.path().to_path_buf(),
            state: AppState::new(store, wiki).with_raw(Some(raw)),
            _repo: repo,
            _data: data,
        }
    }

    fn hook(
        harness: &Harness,
        event: &str,
        extra: serde_json::Value,
    ) -> anamnesis_hooks::ParsedHook {
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
            &harness.state.wiki.lock(),
            harness.state.raw.as_deref(),
            &hook(harness, event, extra),
            now(),
        )
        .expect("ingest")
    }

    #[test]
    fn an_excluded_file_reaches_neither_the_index_nor_the_spool() {
        // The point of the setting: a project says `.env` is not to be
        // remembered, and no copy of it exists anywhere afterwards — not in
        // the index, not in the transcript that outlives the index.
        let harness = harness_with("\n[capture]\nignore_paths = [\".env\"]\n");

        run(&harness, "SessionStart", json!({"source": "startup"}));
        let secret = run(
            &harness,
            "PostToolUse",
            json!({
                "tool_name": "Read",
                "tool_input": {"file_path": ".env", "content": "TOKEN=hunter2"}
            }),
        );
        run(
            &harness,
            "PostToolUse",
            json!({
                "tool_name": "Edit",
                "tool_input": {"file_path": "src/lib.rs"}
            }),
        );

        let observations = harness
            .state
            .store
            .observations(secret.session_id)
            .expect("observations");
        let bodies: Vec<&str> = observations.iter().map(|o| o.body.as_str()).collect();
        assert!(
            !bodies.iter().any(|body| body.contains(".env")),
            "an excluded path was indexed anyway: {bodies:?}"
        );
        assert!(bodies.iter().any(|body| body.contains("src/lib.rs")));

        let scope = resolve_scope(&harness.cwd).expect("scope");
        let session = harness
            .state
            .store
            .load_session(secret.session_id)
            .expect("load")
            .expect("session exists");
        let spooled = harness
            .state
            .raw
            .as_deref()
            .expect("spool")
            .read_session(&scope.scope, &session)
            .expect("read spool");
        assert!(
            !spooled
                .iter()
                .any(|record| format!("{record:?}").contains(".env")),
            "an excluded path was spooled anyway"
        );
    }

    #[test]
    fn exclusions_do_not_stop_a_session_from_starting_or_ending() {
        // Only the events naming an excluded file are dropped. A session made
        // entirely of them still has to open and close, or the next session
        // inherits an open session that never ends.
        let harness = harness_with("\n[capture]\nignore_paths = [\"secrets/**\"]\n");

        run(&harness, "SessionStart", json!({"source": "startup"}));
        run(
            &harness,
            "PostToolUse",
            json!({"tool_name": "Read", "tool_input": {"file_path": "secrets/key.pem"}}),
        );
        let end = run(&harness, "SessionEnd", json!({"reason": "clear"}));

        let session = harness
            .state
            .store
            .load_session(end.session_id)
            .expect("load")
            .expect("session exists");
        assert!(session.ended_at.is_some());
    }

    #[test]
    fn a_project_without_exclusions_captures_everything() {
        let harness = harness();

        let ingested = run(
            &harness,
            "PostToolUse",
            json!({"tool_name": "Read", "tool_input": {"file_path": ".env"}}),
        );

        let observations = harness
            .state
            .store
            .observations(ingested.session_id)
            .expect("observations");
        assert!(
            observations
                .iter()
                .any(|o| o.body.as_str().contains(".env")),
            "nothing is excluded until a project asks for it"
        );
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
            .lock()
            .read_page(&scope.scope, &path)
            .expect("page readable");
        assert!(read.body.contains("wire up the storage layer"));
        assert!(read.body.contains("crates/store/src/ops.rs"));
        assert_eq!(
            harness
                .state
                .store
                .page_count(scope.project_id)
                .expect("count"),
            1
        );
        assert!(harness.state.wiki.lock().commit_count().expect("commits") >= 1);
    }

    #[test]
    fn a_session_page_names_what_it_was_about() {
        // Consolidation used to name nothing at all, so one of the four
        // retrieval streams was blind to every page the system wrote itself.
        let harness = harness();

        run(&harness, "SessionStart", json!({"source": "startup"}));
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "add the provider trait"}),
        );
        run(
            &harness,
            "PostToolUse",
            json!({
                "tool_name": "Edit",
                "tool_input": {"file_path": "crates/anamnesis-llm/src/lib.rs"},
                "tool_response": {"success": true}
            }),
        );
        let end = run(&harness, "SessionEnd", json!({"reason": "clear"}));

        let scope = resolve_scope(&harness.cwd).expect("scope");
        let page = end.page.expect("a page was written");
        let path = anamnesis_core::page::PagePath::parse(&page).expect("path");

        // In the markdown, which is the source of truth …
        let written = harness
            .state
            .wiki
            .lock()
            .read_page(&scope.scope, &path)
            .expect("page readable");
        let named: Vec<&str> = written
            .frontmatter
            .entities
            .iter()
            .map(|entity| entity.as_str())
            .collect();
        assert!(named.contains(&"lib.rs"), "got {named:?}");

        // … and in the index, without anyone having to rebuild it.
        let indexed: i64 = harness
            .state
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM page_entities pe
                 JOIN entities e ON e.id = pe.entity_id
                 WHERE e.name = 'lib.rs'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(indexed, 1);
    }

    #[test]
    fn a_link_in_a_session_page_reaches_the_index_without_a_rebuild() {
        // The live path and a rebuild have to produce the same index. They
        // did not: `reindex` extracted a page's wikilinks and the server did
        // not, so link-neighbour retrieval was blind to every page the system
        // wrote itself until somebody happened to rebuild.
        let harness = harness();
        let scope = resolve_scope(&harness.cwd).expect("scope");

        // A page worth linking to, written the ordinary way.
        let target =
            anamnesis_core::page::PagePath::parse("decisions/0001-storage.md").expect("path");
        let page = anamnesis_core::page::Page::new(
            scope.project_id,
            target.clone(),
            anamnesis_core::page::Frontmatter::new("Storage engine", Vec::new())
                .expect("frontmatter"),
            "SQLite, because the index is disposable.",
        );
        harness
            .state
            .wiki
            .lock()
            .write_page(&scope.scope, &page, "write")
            .expect("write");
        harness
            .state
            .store
            .upsert_project(&scope, now())
            .expect("project");
        harness
            .state
            .store
            .upsert_page(&page, now())
            .expect("upsert");

        run(&harness, "SessionStart", json!({"source": "startup"}));
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "follow up on [[decisions/0001-storage.md]] and finish the index"}),
        );
        let end = run(&harness, "SessionEnd", json!({"reason": "clear"}));

        let session_page = end.page.expect("a page was written");
        let session_id = anamnesis_core::ids::PageId::derive(
            scope.project_id,
            &anamnesis_core::page::PagePath::parse(&session_page).expect("path"),
        );

        let resolved: Option<String> = harness
            .state
            .store
            .connection()
            .query_row(
                "SELECT to_page_id FROM page_links WHERE from_page_id = ?1",
                [session_id.to_string()],
                |row| row.get(0),
            )
            .expect("a link row");
        assert_eq!(
            resolved,
            Some(page.id.to_string()),
            "the link is indexed and resolved to the page it names"
        );
    }

    #[test]
    fn the_next_session_receives_the_handoff_once() {
        let harness = harness();
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "do the thing"}),
        );
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
            harness
                .state
                .store
                .page_count(scope.project_id)
                .expect("count"),
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
        let path = anamnesis_core::page::PagePath::parse(&end.page.expect("page")).expect("path");
        let read = harness
            .state
            .wiki
            .lock()
            .read_page(&scope.scope, &path)
            .expect("read");
        assert!(!read.body.contains("wJalrXUtnFEMIK7MDENG"));
    }

    /// Every line the spool holds for this project, across every session.
    fn spooled(harness: &Harness) -> Vec<anamnesis_store::RawRecord> {
        let spool = harness.state.raw.as_deref().expect("spool");
        spool
            .files()
            .expect("files")
            .iter()
            .flat_map(|path| spool.read_file(path).expect("read"))
            .collect()
    }

    #[test]
    fn every_captured_observation_reaches_the_spool() {
        let harness = harness();
        run(&harness, "SessionStart", json!({"source": "startup"}));
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "wire up the storage layer"}),
        );
        run(&harness, "SessionEnd", json!({}));

        let records = spooled(&harness);
        let sessions = records
            .iter()
            .filter(|r| matches!(r, anamnesis_store::RawRecord::Session(_)))
            .count();
        let observations: Vec<String> = records
            .iter()
            .filter_map(|r| match r {
                anamnesis_store::RawRecord::Observation(o) => Some(o.body.as_str().to_owned()),
                anamnesis_store::RawRecord::Session(_) => None,
            })
            .collect();

        assert_eq!(sessions, 1, "one header for the one session");
        assert_eq!(observations.len(), 3, "start, prompt, and end");
        assert!(
            observations
                .iter()
                .any(|body| body.contains("wire up the storage layer"))
        );
    }

    #[test]
    fn the_spool_survives_the_index_being_deleted() {
        // The whole point of the spool: the wiki keeps the compiled page, but
        // the observations it was compiled from used to live only in SQLite.
        let harness = harness();
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "the raw material"}),
        );

        let before = spooled(&harness);
        drop(harness.state.store.connection());

        assert!(
            before.iter().any(|r| match r {
                anamnesis_store::RawRecord::Observation(o) =>
                    o.body.as_str().contains("the raw material"),
                anamnesis_store::RawRecord::Session(_) => false,
            }),
            "the observation is on disk independently of the index"
        );
    }

    #[test]
    fn secrets_never_reach_the_spool_either() {
        // The spool outlives the database, so an unredacted secret landing
        // here would be the most durable copy of it in the system.
        let harness = harness();
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "deploy using AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENG"}),
        );

        let spool = harness.state.raw.as_deref().expect("spool");
        for path in spool.files().expect("files") {
            let text = std::fs::read_to_string(&path).expect("read");
            assert!(
                !text.contains("wJalrXUtnFEMIK7MDENG"),
                "a secret reached {}",
                path.display()
            );
        }
    }

    #[test]
    fn a_second_session_supersedes_an_unread_handoff() {
        let harness = harness();

        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "first task"}),
        );
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
            &harness.state.wiki.lock(),
            harness.state.raw.as_deref(),
            &parse(&payload),
            now(),
        )
        .expect("ingest");
        payload["hook_event_name"] = json!("SessionEnd");
        ingest(
            &harness.state.store,
            &harness.state.wiki.lock(),
            harness.state.raw.as_deref(),
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
    fn a_payload_carrying_a_byte_order_mark_is_still_read() {
        // PowerShell adds one when piping into a native process. Without this,
        // every event from a Windows hook is silently rejected.
        let value = parse_payload("\u{feff}{\"session_id\":\"s\"}").expect("parsed");
        assert_eq!(value["session_id"], "s");
    }

    #[test]
    fn an_empty_payload_is_reported_rather_than_guessed_at() {
        assert!(matches!(
            parse_payload("   \n"),
            Err(WebError::BadRequest(_))
        ));
    }

    #[test]
    fn a_payload_without_a_cwd_is_refused() {
        let harness = harness();
        let payload = json!({"session_id": "s", "hook_event_name": "SessionStart"});
        let hook = anamnesis_hooks::parse(&AgentKind::ClaudeCode, &payload).expect("parse");
        let result = ingest(
            &harness.state.store,
            &harness.state.wiki.lock(),
            harness.state.raw.as_deref(),
            &hook,
            now(),
        );
        assert!(matches!(result, Err(WebError::BadRequest(_))));
    }

    /// A provider that answers from a script and remembers what it was asked,
    /// so the wiring can be tested without a network or a key.
    struct Fake {
        reply: Option<serde_json::Value>,
        seen: Mutex<Option<String>>,
    }

    impl Fake {
        fn answering(reply: serde_json::Value) -> Self {
            Self {
                reply: Some(reply),
                seen: Mutex::new(None),
            }
        }

        fn broken() -> Self {
            Self {
                reply: None,
                seen: Mutex::new(None),
            }
        }

        fn prompt(&self) -> String {
            self.seen.lock().clone().unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl Provider for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn model(&self) -> &str {
            "fake-1"
        }

        async fn complete(
            &self,
            request: &anamnesis_llm::Completion,
        ) -> Result<anamnesis_llm::CompletionOutput, anamnesis_llm::LlmError> {
            *self.seen.lock() = Some(request.user.clone());
            match &self.reply {
                Some(json) => Ok(anamnesis_llm::CompletionOutput {
                    json: json.clone(),
                    model: "fake-1".to_owned(),
                    input_tokens: 0,
                    output_tokens: 0,
                }),
                None => Err(anamnesis_llm::LlmError::Config("no model".to_owned())),
            }
        }
    }

    /// The budgets every test uses, around whichever provider it supplies.
    fn settings(provider: Arc<dyn Provider>) -> LlmSettings {
        LlmSettings {
            provider,
            max_input_tokens: 6_500,
            max_output_tokens: 2_000,
        }
    }

    /// Record a small session without closing it, and hand back its scope.
    fn recorded(
        harness: &Harness,
    ) -> (
        anamnesis_core::scope::ResolvedScope,
        anamnesis_core::ids::SessionId,
    ) {
        record(
            &harness.state.store,
            harness.state.raw.as_deref(),
            &hook(harness, "SessionStart", json!({"source": "startup"})),
            now(),
        )
        .expect("start");
        record(
            &harness.state.store,
            harness.state.raw.as_deref(),
            &hook(
                harness,
                "UserPromptSubmit",
                json!({"prompt": "wire up the llm provider"}),
            ),
            now(),
        )
        .expect("prompt")
    }

    #[tokio::test]
    async fn a_configured_model_writes_the_page() {
        let harness = harness();
        let (scope, session_id) = recorded(&harness);

        let provider = Arc::new(Fake::answering(json!({
            "title": "LLM provider wired in",
            "body": "## Why. The deterministic path needed a second opinion.",
            "handoff": "The provider is wired; nothing else was touched.",
        })));

        let page = finalize_with_llm(
            &harness.state.store,
            &harness.state.wiki,
            &scope,
            session_id,
            now(),
            &settings(provider.clone()),
        )
        .await
        .expect("finalized")
        .expect("a page");

        let path = anamnesis_core::page::PagePath::parse(&page).expect("path");
        let read = harness
            .state
            .wiki
            .lock()
            .read_page(&scope.scope, &path)
            .expect("page readable");

        assert!(
            read.body
                .contains("The deterministic path needed a second opinion")
        );
        // And not the counted page, which would mean the model was skipped.
        assert!(!read.body.contains("Compiled without a model"));

        let handoff = claim_handoff(
            &harness.state.store,
            &harness.cwd,
            &AgentKind::ClaudeCode,
            "session-next",
            now(),
        )
        .expect("claim")
        .expect("a handoff");
        assert!(handoff.contains("The provider is wired"));
    }

    #[tokio::test]
    async fn a_broken_model_does_not_cost_the_session_its_page() {
        let harness = harness();
        let (scope, session_id) = recorded(&harness);

        let page = finalize_with_llm(
            &harness.state.store,
            &harness.state.wiki,
            &scope,
            session_id,
            now(),
            &settings(Arc::new(Fake::broken())),
        )
        .await
        .expect("finalized")
        .expect("a page");

        let path = anamnesis_core::page::PagePath::parse(&page).expect("path");
        let read = harness
            .state
            .wiki
            .lock()
            .read_page(&scope.scope, &path)
            .expect("page readable");
        assert!(read.body.contains("Compiled without a model"));
        assert!(read.body.contains("wire up the llm provider"));
    }

    #[tokio::test]
    async fn a_projects_preferences_page_reaches_the_model() {
        let harness = harness();
        let (scope, session_id) = recorded(&harness);

        let preferences = harness.state.wiki.lock().locate(
            &scope.scope,
            &anamnesis_core::page::PagePath::parse(anamnesis_consolidate::PREFERENCES_PAGE)
                .expect("path"),
        );
        std::fs::create_dir_all(preferences.parent().expect("parent")).expect("dir");
        std::fs::write(&preferences, "Always name the migration numbers.").expect("write");

        let provider = Arc::new(Fake::answering(json!({
            "title": "t",
            "body": "b",
            "handoff": "h",
        })));

        finalize_with_llm(
            &harness.state.store,
            &harness.state.wiki,
            &scope,
            session_id,
            now(),
            &settings(provider.clone()),
        )
        .await
        .expect("finalized");

        assert!(
            provider
                .prompt()
                .contains("Always name the migration numbers")
        );
    }

    #[tokio::test]
    async fn a_missing_preferences_page_is_not_an_error() {
        let harness = harness();
        let (scope, session_id) = recorded(&harness);
        let provider = Arc::new(Fake::answering(
            json!({"title": "t", "body": "b", "handoff": "h"}),
        ));

        let page = finalize_with_llm(
            &harness.state.store,
            &harness.state.wiki,
            &scope,
            session_id,
            now(),
            &settings(provider.clone()),
        )
        .await
        .expect("finalized");

        assert!(page.is_some());
        assert!(!provider.prompt().contains("Project preferences"));
    }

    // ---------------------------------------------------------------
    // The guard. Exercised through the real router, because what is
    // interesting here is the wiring — which routes the layer covers, and
    // what a request that never reaches a handler leaves behind.
    // ---------------------------------------------------------------

    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    /// A server that accepts exactly these named secrets.
    fn guarded(harness: &Harness, tokens: &str) -> AppState {
        harness
            .state
            .clone()
            .with_auth(Auth::parse(None, Some(tokens)).expect("tokens"))
    }

    async fn send(state: &AppState, request: HttpRequest<Body>) -> Response {
        router(state.clone())
            .oneshot(request)
            .await
            .expect("routed")
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn with_token(builder: axum::http::request::Builder, token: Option<&str>) -> HttpRequest<Body> {
        let builder = match token {
            Some(token) => builder.header("authorization", format!("Bearer {token}")),
            None => builder,
        };
        builder.body(Body::empty()).expect("request")
    }

    fn hook_request(harness: &Harness, event: &str, token: Option<&str>) -> HttpRequest<Body> {
        let payload = json!({
            "session_id": "session-guard",
            "hook_event_name": event,
            "cwd": harness.cwd.to_string_lossy(),
        });
        let builder = HttpRequest::builder()
            .method("POST")
            .uri("/hook?agent=claude-code")
            .header("content-type", "application/json");
        let builder = match token {
            Some(token) => builder.header("authorization", format!("Bearer {token}")),
            None => builder,
        };
        builder
            .body(Body::from(payload.to_string()))
            .expect("request")
    }

    fn project(harness: &Harness) -> anamnesis_core::ids::ProjectId {
        resolve_scope(&harness.cwd).expect("scope").project_id
    }

    /// The default, and the reason absence is not an error: an install that
    /// predates tokens keeps delivering events exactly as it did.
    #[tokio::test]
    async fn a_server_with_no_tokens_accepts_a_hook_that_carries_none() {
        let harness = harness();
        let response = send(&harness.state, hook_request(&harness, "SessionStart", None)).await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            harness
                .state
                .store
                .session_count(project(&harness))
                .expect("count"),
            1
        );
    }

    /// The point of the whole module: an unauthenticated event is not recorded,
    /// not spooled, and not half-recorded either — the session row a hook would
    /// have created is not there, because the request never reached a handler.
    #[tokio::test]
    async fn a_hook_without_a_token_records_nothing() {
        let harness = harness();
        let state = guarded(&harness, "alice=alpha");

        let response = send(&state, hook_request(&harness, "SessionStart", None)).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer"),
            "a 401 has to say what it wants"
        );
        assert_eq!(
            state.store.session_count(project(&harness)).expect("count"),
            0
        );
    }

    #[tokio::test]
    async fn a_hook_with_the_right_token_is_recorded_as_before() {
        let harness = harness();
        let state = guarded(&harness, "alice=alpha");

        let response = send(
            &state,
            hook_request(&harness, "SessionStart", Some("alpha")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            state.store.session_count(project(&harness)).expect("count"),
            1
        );
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused_and_told_which_variable_to_check() {
        let harness = harness();
        let state = guarded(&harness, "alice=alpha");

        let response = send(&state, hook_request(&harness, "SessionStart", Some("beta"))).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = body_of(response).await;
        assert!(body.contains(auth::TOKEN_ENV), "{body}");
        // Never the token that was presented: this text lands on someone's
        // stderr, and stderr ends up in issues.
        assert!(!body.contains("beta"), "{body}");
    }

    /// `status` distinguishes "the server is down" from "the server refuses
    /// this machine", and it can only do that if liveness stays answerable
    /// without a token.
    #[tokio::test]
    async fn health_answers_without_a_token() {
        let harness = harness();
        let state = guarded(&harness, "alice=alpha");

        let request = HttpRequest::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let response = send(&state, request).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_of(response).await, "ok\n");
    }

    #[tokio::test]
    async fn whoami_says_who_the_token_belongs_to() {
        let harness = harness();
        let state = guarded(&harness, "alice=alpha,bob=beta");

        let response = send(
            &state,
            with_token(HttpRequest::builder().uri("/whoami"), Some("beta")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&body_of(response).await).expect("json");
        assert_eq!(body["auth"], "token");
        assert_eq!(body["operator"], "bob");
    }

    #[tokio::test]
    async fn whoami_admits_when_the_server_is_open() {
        let harness = harness();
        let response = send(
            &harness.state,
            with_token(HttpRequest::builder().uri("/whoami"), None),
        )
        .await;

        let body: serde_json::Value = serde_json::from_str(&body_of(response).await).expect("json");
        assert_eq!(body["auth"], "open");
        assert_eq!(body["operator"], serde_json::Value::Null);
    }

    /// A handoff is single-use, so a refused request must not be a use. The
    /// layer running before the handler is what guarantees it; this is the
    /// test that would notice if the guard were ever moved inside.
    #[tokio::test]
    async fn a_refused_request_does_not_spend_the_handoff() {
        let harness = harness();
        run(&harness, "SessionStart", json!({"source": "startup"}));
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "make it work"}),
        );
        run(
            &harness,
            "PostToolUse",
            json!({"tool_name": "Edit", "tool_input": {"file_path": "src/lib.rs"}}),
        );
        run(&harness, "SessionEnd", json!({"reason": "clear"}));

        let state = guarded(&harness, "alice=alpha");
        let project = project(&harness);
        assert!(
            state
                .store
                .peek_handoff(project, None)
                .expect("peek")
                .is_some(),
            "the session should have left a handoff to lose"
        );

        let uri = format!(
            "/handoff?agent=claude-code&session_id=next&cwd={}",
            percent_encode(&harness.cwd.to_string_lossy())
        );
        let response = send(&state, with_token(HttpRequest::builder().uri(uri), None)).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            state
                .store
                .peek_handoff(project, None)
                .expect("peek")
                .is_some(),
            "a refused request spent the handoff it was refused"
        );
    }

    /// `route_layer`, not `layer`: a path this server does not serve is a 404.
    /// A 401 there would tell a stranger which paths exist.
    #[tokio::test]
    async fn an_unknown_path_is_not_challenged_for_a_token() {
        let harness = harness();
        let state = guarded(&harness, "alice=alpha");

        let request = HttpRequest::builder()
            .uri("/admin")
            .body(Body::empty())
            .expect("request");

        assert_eq!(send(&state, request).await.status(), StatusCode::NOT_FOUND);
    }

    /// Percent-encode a path so it survives being a query parameter, which on
    /// Windows means encoding the drive colon and the backslashes.
    fn percent_encode(value: &str) -> String {
        value
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (byte as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect()
    }
}
