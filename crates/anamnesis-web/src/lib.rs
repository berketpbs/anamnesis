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
use anamnesis_llm::{Embedder, Provider};
use anamnesis_store::{RawSpool, Store};
use anamnesis_wiki::Wiki;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use jiff::Timestamp;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio_util::task::TaskTracker;

pub mod auth;
pub mod improve;
mod pipeline;
pub mod reap;
pub mod ui;
pub mod watch;

pub use auth::{Auth, Identity};
pub use pipeline::{
    Consolidation, Ingested, ProbeReport, claim_handoff, finalize, finalize_with_llm, ingest,
    probe, record,
};

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
    /// The embedder, when one is enabled. Every page this server writes gets a
    /// vector, so the stream covers the memory rather than the corner of it an
    /// agent happened to write through MCP.
    pub embedder: Option<Arc<dyn Embedder>>,
    /// Work that outlives the request that started it.
    ///
    /// Only *finite* work goes here — a session being summarised by a model,
    /// which the response no longer waits for. The scheduler and the watcher
    /// are loops that never finish, so tracking them would turn a shutdown
    /// that waits into a shutdown that hangs.
    pub tasks: TaskTracker,
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
            embedder: None,
            tasks: TaskTracker::new(),
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

    /// Embed the pages this server writes, when an embedder is enabled.
    pub fn with_embedder(mut self, embedder: Option<Arc<dyn Embedder>>) -> Self {
        self.embedder = embedder;
        self
    }
}

/// Largest hook payload the server will read.
///
/// Axum's default is two megabytes, which is smaller than a single ordinary
/// event: one `Read` of a large file, or a search over a big tree, produces a
/// tool response past it. What the server *keeps* of a body is 16 KB, cut
/// after parsing, so refusing the request outright rejects an event it was
/// about to shorten anyway — and leaves the hook holding a payload no retry
/// can ever deliver, at the head of a queue that stops there.
///
/// It cannot be unbounded either. The body is buffered whole and scanned for
/// secrets before any of it is kept, and both costs are the body's size. This
/// ceiling is eight times the largest real payload measured here and still
/// small enough that a request cannot be a memory attack.
const MAX_HOOK_BODY: usize = 16 * 1024 * 1024;

/// Build the router.
///
/// `/health` is outside the guard on purpose. It says only that an anamnesis
/// server is listening — which the open port already says — and keeping it
/// answerable is what lets `anamnesis status` tell "the server is down" apart
/// from "the server is up and does not accept your token". Collapsing those two
/// into one silence is how a person spends an afternoon restarting a server
/// that was running the whole time.
///
/// `ui` mounts the wiki browser. It is the one part of this surface that can
/// read the whole of memory — the API delivers a handoff and accepts events,
/// and neither hands back an arbitrary page — so it is also the one part worth
/// being able to switch off on a server other people can reach.
pub fn router(state: AppState, ui: bool) -> Router {
    let guarded = Router::new()
        .route(
            "/hook",
            post(receive_hook).layer(DefaultBodyLimit::max(MAX_HOOK_BODY)),
        )
        .route("/handoff", get(deliver_handoff))
        .route("/whoami", get(whoami))
        // `route_layer`, not `layer`: a request for a path this server does not
        // serve should be a 404, not a 401 that implies the path exists.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ));

    let mut app = Router::new().route("/health", get(health)).merge(guarded);
    if ui {
        app = app.merge(ui::routes(&state));
    }
    app.with_state(state)
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

/// The same guard, for requests a person's browser makes.
///
/// It differs from [`require_token`] in exactly two ways, and both are about
/// what a browser can be asked to do. It also accepts the token as an HTTP
/// Basic password, because a browser will not attach a bearer token to a link
/// somebody clicked but will ask for a password and remember it. And it
/// answers with a page and a `WWW-Authenticate: Basic` challenge instead of a
/// line of text, so the prompt actually appears.
///
/// The API keeps the header-only rule: a credential a browser sends on its own
/// must not be able to authorise `POST /hook`, or a page on another site could
/// make the browser write to somebody's memory.
async fn require_browser_token(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    match state.auth.authenticate_browser(header) {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(rejection) => {
            tracing::warn!(
                path = %request.uri().path(),
                reason = ?rejection,
                "rejected an unauthenticated browser request"
            );
            ui::challenge(&rejection.message())
        }
    }
}

/// What the server makes of the caller's token, and how it compiles memory.
#[derive(Debug, Serialize)]
struct WhoAmI {
    /// `open` when no token is required, `token` when one was accepted.
    auth: &'static str,
    /// The operator the token belongs to, when it names one.
    operator: Option<String>,
    /// The model sessions are summarised with. `null` means they are counted.
    consolidation: Option<String>,
    /// The model pages are embedded with. `null` means vector search is off.
    embedding: Option<String>,
}

/// Report the caller's identity back to them, and what this server does.
///
/// The endpoint exists for the question `status` has to answer — "does this
/// machine's token work?" — which no other route can answer without side
/// effects: `/handoff` consumes a handoff, and `/hook` records an event.
///
/// The two model fields ride along because the server is the only thing that
/// knows them. Consolidation happens here, from this process's environment,
/// and a client reading its *own* environment would confidently report a model
/// the server does not have — or none when the server has one. That is not a
/// hypothetical: this system spent a week writing counted summaries while the
/// only line that said so was a startup banner printed to a hidden console.
async fn whoami(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Json<WhoAmI> {
    Json(WhoAmI {
        auth: if identity.is_anonymous() {
            "open"
        } else {
            "token"
        },
        operator: identity.operator().map(ToString::to_string),
        consolidation: state
            .llm
            .as_ref()
            .map(|settings| settings.provider.model().to_owned()),
        embedding: state
            .embedder
            .as_ref()
            .map(|embedder| embedder.model().to_owned()),
    })
}

/// What a running server does beyond answering the API.
///
/// A struct rather than two positional booleans: the call site is a long way
/// from this definition, and `true, false` there says nothing about which
/// switch is which.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// Index pages edited outside anamnesis as they are saved.
    pub watch_wiki: bool,
    /// Mount the wiki browser.
    pub ui: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            watch_wiki: true,
            ui: true,
        }
    }
}

/// Serve until the process ends.
pub async fn serve(
    bind: SocketAddr,
    state: AppState,
    options: ServeOptions,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "anamnesis listening");

    // The server is the only part of the system that runs for longer than one
    // command, so it is where a schedule can live. It costs nothing until a
    // project turns one on: every tick over a fleet that has not asked for
    // auto-improve is one query and a list of reasons why not.
    tokio::spawn(improve::run_scheduler(state.clone()));

    // Unconditional, unlike the scheduler above: auto-improve is a thing a
    // project opts into, but a session nobody closed is a fault, and leaving
    // one unswept because a marker file said nothing would be answering the
    // wrong question.
    tokio::spawn(reap::run_reaper(state.clone()));

    // On by default, unlike the scheduler, and the difference is what each one
    // does: auto-improve makes decisions about someone's memory, so it waits to
    // be asked. The watcher only makes the index say what the wiki already
    // says. A blocking task because everything it touches — SQLite, git, the
    // wiki mutex — is synchronous.
    if options.watch_wiki {
        let watching = state.clone();
        // Spawned and then *awaited*, which the handle being dropped would
        // not do. `watch::run` is a loop: it returning at all means the wiki
        // has stopped being watched, and a panic means the same thing with
        // less warning. Either way the server carries on looking healthy
        // while the banner it printed at startup — "wiki edits: watched" —
        // has quietly stopped being true.
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || watch::run(watching)).await {
                Ok(()) => tracing::warn!(
                    "the wiki watcher stopped; pages edited by hand now reach the index only through `anamnesis reindex`"
                ),
                Err(error) => tracing::error!(
                    %error,
                    "the wiki watcher panicked; pages edited by hand now reach the index only through `anamnesis reindex`"
                ),
            }
        });
    }

    let tasks = state.tasks.clone();

    // The signal has to come back out of the shutdown future, because *which*
    // signal it was decides how long there is left to finish up.
    let (signalled, stop) = tokio::sync::oneshot::channel();
    axum::serve(listener, router(state, options.ui))
        .with_graceful_shutdown(async move {
            let reason = stopped().await;
            // Written before the waiting starts rather than after it: when the
            // operating system is the one counting, this line is the last
            // thing the process is certain to get to say.
            tracing::info!(cause = reason.cause, "stopping");
            let _ = signalled.send(reason);
        })
        .await?;

    // The listener is closed and every request has been answered; what is left
    // is the work those requests started and no longer wait for.
    let stop = stop.await.unwrap_or(Stop::UNKNOWN);
    finish_in_flight(&tasks, stop.grace).await;
    tracing::info!(cause = stop.cause, "anamnesis stopped");
    Ok(())
}

/// How long a stopping server waits for summaries already being written.
///
/// Not the model's timeout, which is 90 seconds by default: `docker stop`
/// sends SIGKILL 10 seconds after SIGTERM, so a longer wait would mostly be a
/// promise the container runtime breaks. Fifteen seconds covers a model that
/// is nearly done and an operator's Ctrl-C, and says plainly what it gave up
/// on when it does.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

/// The same wait, when the operating system is holding the stopwatch.
///
/// Windows gives a console process about five seconds to handle the close,
/// logoff and shutdown events and then kills it whatever it is doing. Waiting
/// the usual fifteen there would not be generous, it would be a promise
/// Windows breaks: the process dies mid-wait and the line naming the summaries
/// it abandoned — the reason for waiting at all — is never written. Four
/// seconds leaves room to write it.
#[cfg(windows)]
const OS_DEADLINE_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// Why the server is stopping, and how long it has left to finish up.
///
/// The two travel together because they are not independent: a signal that
/// arrives with an operating-system deadline attached cannot be given the same
/// grace as one that does not.
#[derive(Debug, Clone, Copy)]
struct Stop {
    /// What asked the server to stop.
    ///
    /// It is logged because for a process nobody is watching this is the only
    /// account of why it went away, and the absence of that account is what
    /// made this repository's own four-day silence so hard to explain: that
    /// the server was gone could be seen, that it had been killed when a
    /// console closed could not.
    cause: &'static str,
    /// How long work already in flight may take before it is abandoned.
    grace: std::time::Duration,
}

impl Stop {
    /// What to assume when `serve` returns for a reason that was not a signal.
    ///
    /// Reachable only if the shutdown future is dropped without resolving, so
    /// it says what it knows — nothing — rather than naming a signal that
    /// never arrived.
    const UNKNOWN: Self = Self {
        cause: "the listener stopped",
        grace: SHUTDOWN_GRACE,
    };
}

/// Resolve when the operating system asks this process to stop.
///
/// Every way it can be asked, because they arrive from different places and
/// mean the same thing here: Ctrl-C from a terminal, SIGTERM from `docker
/// stop`, systemd or a supervisor, and on Windows the console window being
/// closed, the session logging off, or the machine shutting down.
///
/// Windows is not a footnote. `ctrl_c` covers CTRL_C_EVENT and nothing else,
/// so until the console events were registered a server whose window was
/// closed took the abrupt path — which is precisely how this project's own
/// memory has died, on the one platform where closing the window is how people
/// stop things.
async fn stopped() -> Stop {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
        Stop {
            cause: "interrupted",
            grace: SHUTDOWN_GRACE,
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
                Stop {
                    cause: "asked to terminate",
                    grace: SHUTDOWN_GRACE,
                }
            }
            // A process that cannot register the handler still stops on
            // Ctrl-C; refusing to serve over it would be worse.
            Err(error) => {
                tracing::warn!(%error, "could not listen for SIGTERM");
                std::future::pending::<Stop>().await
            }
        }
    };

    #[cfg(windows)]
    let terminate = async {
        // All three, because they are one event with three names and one
        // deadline. A server that handled only the window closing would still
        // go silently every time the machine restarts.
        let registered = (|| {
            Ok::<_, std::io::Error>((
                tokio::signal::windows::ctrl_close()?,
                tokio::signal::windows::ctrl_logoff()?,
                tokio::signal::windows::ctrl_shutdown()?,
            ))
        })();

        let (mut close, mut logoff, mut shutdown) = match registered {
            Ok(signals) => signals,
            // Same reasoning as SIGTERM above: it still stops on Ctrl-C.
            Err(error) => {
                tracing::warn!(%error, "could not listen for the console events");
                return std::future::pending::<Stop>().await;
            }
        };

        let cause = tokio::select! {
            _ = close.recv() => "the console was closed",
            _ = logoff.recv() => "the session logged off",
            _ = shutdown.recv() => "the system is shutting down",
        };
        Stop {
            cause,
            grace: OS_DEADLINE_GRACE,
        }
    };

    #[cfg(not(any(unix, windows)))]
    let terminate = std::future::pending::<Stop>();

    tokio::select! {
        stop = interrupt => stop,
        stop = terminate => stop,
    }
}

/// Wait for tracked work, up to `grace`. Returns whether it all finished.
///
/// Separate from [`serve`] because the interesting half is what happens when
/// the wait runs out, and a signal is a poor thing to write a test around.
async fn finish_in_flight(tasks: &TaskTracker, grace: std::time::Duration) -> bool {
    tasks.close();
    if tasks.is_empty() {
        return true;
    }

    tracing::info!(
        summaries = tasks.len(),
        "waiting for sessions still being summarised"
    );
    match tokio::time::timeout(grace, tasks.wait()).await {
        Ok(()) => true,
        Err(_) => {
            // Named, because the alternative is a session that ended and left
            // no page with nothing anywhere saying why. The observations are
            // in the index and the raw spool; only the prose is lost.
            tracing::warn!(
                summaries = tasks.len(),
                seconds = grace.as_secs(),
                "gave up waiting; these sessions end without a summary, though their transcripts are kept"
            );
            false
        }
    }
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
    /// Ask what would be recorded, and record nothing.
    probe: Option<String>,
    /// The sender's own name for this delivery, so a repeat of it is one event.
    event: Option<String>,
}

impl AgentQuery {
    /// Whether the caller asked for a dry run.
    ///
    /// Presence is the signal, so `?probe` and `?probe=1` mean the same
    /// thing, and only an explicit `0` or `false` turns it back off. The
    /// failure being avoided is a diagnostic flag that quietly meant nothing
    /// because a shell wrote it in a shape the parser did not expect — this
    /// endpoint writes, and a probe that silently stopped being one would
    /// record the event it was asked to only describe.
    fn probing(&self) -> bool {
        !matches!(self.probe.as_deref(), None | Some("0") | Some("false"))
    }

    /// The identity the sender gave this delivery.
    ///
    /// An unreadable one is dropped rather than refused, and the event is
    /// recorded under a fresh identifier. The cost of that is a duplicate if
    /// this delivery is repeated; the cost of the alternative is refusing an
    /// event outright over a field no harness sends and only this project's
    /// own hook fills in.
    fn delivery(&self) -> Option<anamnesis_core::ids::ObservationId> {
        let raw = self.event.as_deref()?;
        match raw.parse() {
            Ok(id) => Some(id),
            Err(error) => {
                tracing::warn!(%error, event = raw, "ignoring an unreadable event identifier");
                None
            }
        }
    }

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
    Extension(identity): Extension<Identity>,
    Query(query): Query<AgentQuery>,
    body: String,
) -> Result<Response, WebError> {
    let payload = parse_payload(&body)?;

    let hook = anamnesis_hooks::parse(&query.agent(), &payload)
        .map_err(|error| WebError::BadRequest(error.to_string()))?
        .sent_as(query.delivery());

    if hook.was_redacted() {
        tracing::info!(rules = ?hook.redactions, "redacted secrets from hook payload");
    }

    let now = Timestamp::now();

    // Before every write, because that is the whole promise: a probe reports
    // what the rest of this function would do and leaves the index, the
    // spool, and the waiting handoff exactly as it found them.
    if query.probing() {
        let report = pipeline::probe(
            &state.store,
            &hook,
            now,
            identity.operator(),
            state.llm.is_some(),
        )?;
        return Ok((StatusCode::OK, Json(report)).into_response());
    }

    let Some(settings) = state.llm.clone() else {
        // No model: summarising is counting, which is fast enough to finish
        // while the hook waits.
        let outcome = {
            let wiki = state.wiki.lock();
            ingest(
                &state.store,
                &wiki,
                state.raw.as_deref(),
                &hook,
                state
                    .embedder
                    .as_ref()
                    .map(|embedder| embedder.as_ref() as &dyn anamnesis_core::embedding::Embed),
                now,
                identity.operator(),
            )?
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
    let (scope, session_id) = record(
        &state.store,
        state.raw.as_deref(),
        &hook,
        now,
        identity.operator(),
    )?;

    if hook.kind == EventKind::SessionEnd {
        let background = state.clone();
        // Spawned on the tracker rather than loose, so a server being shut
        // down can wait for the page this session is owed instead of taking
        // it to the grave. The transcript survives either way; the summary is
        // the part nothing rebuilds.
        state.tasks.spawn(async move {
            // The inner spawn is what makes a panic reportable: a task nobody
            // awaits takes its panic with it, and the only trace of one here
            // would be a session that ended and left no page. Every failure in
            // this system that took days to notice had exactly that shape.
            let handle = tokio::spawn(async move {
                let outcome = finalize_with_llm(
                    &background.store,
                    &background.wiki,
                    &scope,
                    session_id,
                    background
                        .embedder
                        .as_ref()
                        .map(|embedder| embedder.as_ref() as &dyn anamnesis_core::embedding::Embed),
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

            if let Err(error) = handle.await {
                // The session stays open and its transcript is whole; what is
                // lost is the summary, and this line is the only place that
                // says so.
                tracing::error!(%error, %session_id, "consolidation panicked");
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
    Extension(identity): Extension<Identity>,
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
        identity.operator(),
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
        run_as(harness, event, extra, None)
    }

    /// The same, attributed to an operator, as an authenticated hook is.
    fn run_as(
        harness: &Harness,
        event: &str,
        extra: serde_json::Value,
        operator: Option<&anamnesis_core::scope::OperatorName>,
    ) -> Ingested {
        ingest(
            &harness.state.store,
            &harness.state.wiki.lock(),
            harness.state.raw.as_deref(),
            &hook(harness, event, extra),
            None,
            now(),
            operator,
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

    /// The safety property behind summarising a session nobody closed: doing
    /// it early is survivable. An agent that goes quiet long enough to be
    /// summarised and then carries on gets its session back, so nothing after
    /// the summary lands in a session that will never be read again.
    #[test]
    fn a_session_that_was_already_summarised_carries_on_where_it_left_off() {
        let harness = harness();
        run(&harness, "SessionStart", json!({"source": "startup"}));
        let first = run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "wire up the storage layer"}),
        );
        let end = run(&harness, "SessionEnd", json!({"reason": "clear"}));
        assert!(end.consolidated, "the session should have been summarised");

        let resumed = run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "and now the retrieval side"}),
        );

        assert_eq!(
            resumed.session_id, first.session_id,
            "it is the same session, not a new one"
        );
        let session = harness
            .state
            .store
            .load_session(resumed.session_id)
            .expect("load")
            .expect("found");
        assert!(session.is_open(), "it should be open again");
        assert!(
            harness
                .state
                .store
                .observations(resumed.session_id)
                .expect("observations")
                .iter()
                .any(|o| o.body.as_str().contains("retrieval side")),
            "what it said after the summary has to be kept"
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
                None,
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
                None,
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
            None,
            now(),
            None,
        )
        .expect("ingest");
        payload["hook_event_name"] = json!("SessionEnd");
        ingest(
            &harness.state.store,
            &harness.state.wiki.lock(),
            harness.state.raw.as_deref(),
            &parse(&payload),
            None,
            now(),
            None,
        )
        .expect("ingest");

        let claimed = claim_handoff(
            &harness.state.store,
            &harness.cwd,
            &AgentKind::ClaudeCode,
            "session-three",
            now(),
            None,
        )
        .expect("claim")
        .expect("something pending");
        assert!(claimed.contains("second task"));
        assert!(!claimed.contains("first task"));
    }

    // ---------------------------------------------------------------
    // Stopping. The response goes out before the page is written, so a
    // server that stops the moment it is asked to loses work nothing
    // rebuilds.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn a_summary_still_being_written_is_waited_for() {
        let harness = harness();
        let provider = Arc::new(
            Fake::answering(json!({
                "title": "Rebuildable index",
                "body": "## What happened\n\nThe index was explained.",
                "handoff": "Nothing pending.",
                "entities": []
            }))
            .slowly(std::time::Duration::from_millis(250)),
        );
        let state = harness
            .state
            .clone()
            .with_llm(Some(settings(provider.clone())));

        send(&state, hook_request(&harness, "SessionStart", None)).await;
        send(&state, hook_request(&harness, "UserPromptSubmit", None)).await;
        let response = send(&state, hook_request(&harness, "SessionEnd", None)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        // The point of the split: the hook is not kept waiting for a model.
        assert_eq!(
            state.store.page_count(project(&harness)).expect("count"),
            0,
            "the response should come back before the page is written"
        );

        assert!(
            finish_in_flight(&state.tasks, std::time::Duration::from_secs(10)).await,
            "the summary had time to finish"
        );
        assert_eq!(
            state.store.page_count(project(&harness)).expect("count"),
            1,
            "the page a stopping server owed this session"
        );
    }

    /// A provider crate is third-party code running in a task nobody awaits.
    /// If it dies, the server must stay up and the session must stay
    /// recoverable — half a summary written over a closed session would be
    /// worse than none.
    #[tokio::test]
    async fn a_provider_that_panics_does_not_take_the_server_with_it() {
        let harness = harness();
        let provider = Arc::new(Fake::exploding());
        let state = harness.state.clone().with_llm(Some(settings(provider)));

        send(&state, hook_request(&harness, "SessionStart", None)).await;
        send(&state, hook_request(&harness, "UserPromptSubmit", None)).await;
        let response = send(&state, hook_request(&harness, "SessionEnd", None)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        // A task that dies still *ends*, so the drain must not be left
        // waiting on it — a panicking provider would otherwise turn every
        // shutdown into the full fifteen seconds.
        assert!(finish_in_flight(&state.tasks, std::time::Duration::from_secs(10)).await);

        // No page, which is the honest outcome, and no session pretending to
        // have been summarised: the transcript is whole and the row is not
        // closed behind a summary that was never written.
        assert_eq!(state.store.page_count(project(&harness)).expect("count"), 0);
        let sessions = state
            .store
            .recent_sessions(project(&harness), 10)
            .expect("sessions");
        assert!(
            sessions.iter().all(|session| session.ended_at.is_none()),
            "the session should not be closed by a consolidation that died"
        );

        // And the server is still answering, which is the whole point.
        let alive = send(&state, hook_request(&harness, "SessionStart", None)).await;
        assert_eq!(alive.status(), StatusCode::ACCEPTED);
    }

    /// A model that has hung must not turn a stop into a hang. The transcript
    /// survives; the summary is the part that is lost, and the log says so.
    #[tokio::test]
    async fn work_that_will_not_finish_does_not_hold_the_shutdown_open() {
        let tasks = TaskTracker::new();
        tasks.spawn(async {
            std::future::pending::<()>().await;
        });

        let finished = finish_in_flight(&tasks, std::time::Duration::from_millis(50)).await;

        assert!(!finished);
    }

    #[tokio::test]
    async fn a_server_with_nothing_in_flight_stops_at_once() {
        let tasks = TaskTracker::new();

        assert!(finish_in_flight(&tasks, std::time::Duration::from_secs(30)).await);
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
            None,
            now(),
            None,
        );
        assert!(matches!(result, Err(WebError::BadRequest(_))));
    }

    /// A provider that answers from a script and remembers what it was asked,
    /// so the wiring can be tested without a network or a key.
    struct Fake {
        reply: Option<serde_json::Value>,
        seen: Mutex<Option<String>>,
        /// How long the provider takes to answer. Zero unless a test needs the
        /// window between the response and the page to be observable.
        takes: std::time::Duration,
        /// Whether the client panics instead of returning.
        explodes: bool,
    }

    impl Fake {
        fn answering(reply: serde_json::Value) -> Self {
            Self {
                reply: Some(reply),
                seen: Mutex::new(None),
                takes: std::time::Duration::ZERO,
                explodes: false,
            }
        }

        fn broken() -> Self {
            Self {
                reply: None,
                seen: Mutex::new(None),
                takes: std::time::Duration::ZERO,
                explodes: false,
            }
        }

        /// The same provider, answering slowly enough to be caught at it.
        fn slowly(mut self, takes: std::time::Duration) -> Self {
            self.takes = takes;
            self
        }

        /// A client that dies rather than answering. Not a hypothetical: a
        /// provider crate is third-party code running inside a task nobody
        /// awaits.
        fn exploding() -> Self {
            Self {
                reply: None,
                seen: Mutex::new(None),
                takes: std::time::Duration::ZERO,
                explodes: true,
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
            assert!(!self.explodes, "provider exploded");
            if !self.takes.is_zero() {
                tokio::time::sleep(self.takes).await;
            }
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
            now(),
            &settings(provider.clone()),
        )
        .await
        .expect("finalized");

        assert!(page.is_some());
        assert!(!provider.prompt().contains("Project preferences"));
    }

    // ---------------------------------------------------------------
    // Attribution: whose session a page describes.
    // ---------------------------------------------------------------

    /// On a shared server this is the difference between a wiki of sessions
    /// and a wiki of *somebody's* sessions.
    #[test]
    fn a_session_page_says_who_ran_the_session() {
        let harness = harness();
        let operator = anamnesis_core::scope::OperatorName::parse("alice").expect("name");
        run_as(&harness, "SessionStart", json!({}), Some(&operator));
        run_as(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "why is the index rebuildable"}),
            Some(&operator),
        );
        let ingested = run_as(&harness, "SessionEnd", json!({}), Some(&operator));

        let path = ingested.page.expect("page");
        let scope = resolve_scope(&harness.cwd).expect("scope");
        let page = harness
            .state
            .wiki
            .lock()
            .read_page(
                &scope.scope,
                &anamnesis_core::page::PagePath::parse(&path).expect("path"),
            )
            .expect("read");

        assert!(page.body.contains("Recorded by alice."), "{}", page.body);
    }

    /// A server with no tokens has no name to write, and "unknown" on every
    /// page of every single-person install is noise standing in for a fact
    /// nobody was missing.
    #[test]
    fn a_session_nobody_was_named_for_says_nothing_about_an_operator() {
        let harness = harness();
        run(&harness, "SessionStart", json!({}));
        run(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "why is the index rebuildable"}),
        );
        let ingested = run(&harness, "SessionEnd", json!({}));

        let path = ingested.page.expect("page");
        let scope = resolve_scope(&harness.cwd).expect("scope");
        let page = harness
            .state
            .wiki
            .lock()
            .read_page(
                &scope.scope,
                &anamnesis_core::page::PagePath::parse(&path).expect("path"),
            )
            .expect("read");

        assert!(!page.body.contains("Recorded by"), "{}", page.body);
    }

    /// The attribution is a fact about the session, so a summary written by a
    /// model carries it exactly as a counted one does — and the model is never
    /// told the name in the first place.
    #[tokio::test]
    async fn a_page_a_model_wrote_carries_the_attribution_too() {
        let harness = harness();
        let operator = anamnesis_core::scope::OperatorName::parse("alice").expect("name");
        run_as(&harness, "SessionStart", json!({}), Some(&operator));
        run_as(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "why is the index rebuildable"}),
            Some(&operator),
        );
        let ingested = run_as(&harness, "SessionEnd", json!({}), Some(&operator));

        let provider = Arc::new(Fake::answering(json!({
            "title": "Rebuildable index",
            "body": "## What happened\n\nThe index was explained.",
            "handoff": "Nothing pending.",
            "entities": []
        })));
        let scope = resolve_scope(&harness.cwd).expect("scope");
        let page = finalize_with_llm(
            &harness.state.store,
            &harness.state.wiki,
            &scope,
            ingested.session_id,
            None,
            now(),
            &settings(provider.clone()),
        )
        .await
        .expect("finalized")
        .expect("page");

        let written = harness
            .state
            .wiki
            .lock()
            .read_page(
                &scope.scope,
                &anamnesis_core::page::PagePath::parse(&page).expect("path"),
            )
            .expect("read");

        assert!(
            written.body.contains("Recorded by alice."),
            "{}",
            written.body
        );
        assert!(!provider.prompt().contains("alice"), "the name is not sent");
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
        router(state.clone(), true)
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

    // ---------------------------------------------------------------
    // Probing. The promise is negative — that nothing happened — so every
    // test here checks the state the request did *not* change.
    // ---------------------------------------------------------------

    /// A probe request, with whatever the caller wants after `probe=`.
    fn probe_request(harness: &Harness, query: &str) -> HttpRequest<Body> {
        let payload = json!({
            "session_id": "session-probe",
            "hook_event_name": "UserPromptSubmit",
            "cwd": harness.cwd.to_string_lossy(),
            "prompt": "is capture alive",
        });
        HttpRequest::builder()
            .method("POST")
            .uri(format!("/hook?agent=claude-code&probe={query}"))
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request")
    }

    #[tokio::test]
    async fn a_probe_says_what_would_happen_and_records_nothing() {
        let harness = harness();

        let response = send(&harness.state, probe_request(&harness, "1")).await;
        assert_eq!(response.status(), StatusCode::OK);

        let report: ProbeReport = serde_json::from_str(&body_of(response).await).expect("report");
        assert!(report.would_record);
        assert_eq!(report.project, "widget");
        assert_eq!(report.event, "user-prompt");
        assert!(!report.session_known, "nothing has recorded this session");
        assert_eq!(report.consolidation, Consolidation::Counted);

        assert_eq!(
            harness
                .state
                .store
                .session_count(project(&harness))
                .expect("count"),
            0,
            "a probe must leave no session behind — the whole reason it exists"
        );
    }

    /// The failure that shaped this: a diagnostic that proves memory works by
    /// taking the note the next session was owed.
    #[tokio::test]
    async fn a_probe_does_not_claim_the_waiting_handoff() {
        let harness = harness();
        run(&harness, "UserPromptSubmit", json!({"prompt": "real work"}));
        run(&harness, "SessionEnd", json!({}));

        let slot = anamnesis_core::handoff::Slot::default();
        let before = harness
            .state
            .store
            .peek_handoff(project(&harness), &slot)
            .expect("peek");
        assert!(before.is_some(), "the fixture should leave a note waiting");

        let response = send(&harness.state, probe_request(&harness, "1")).await;
        let report: ProbeReport = serde_json::from_str(&body_of(response).await).expect("report");
        assert!(report.handoff_waiting, "and the probe should see it");

        assert_eq!(
            harness
                .state
                .store
                .peek_handoff(project(&harness), &slot)
                .expect("peek"),
            before,
            "seeing a handoff must not consume it"
        );
    }

    /// The off switch has to work, or the parameter is decoration.
    #[tokio::test]
    async fn a_probe_switched_off_is_an_ordinary_event() {
        let harness = harness();

        let response = send(&harness.state, probe_request(&harness, "0")).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        assert_eq!(
            harness
                .state
                .store
                .session_count(project(&harness))
                .expect("count"),
            1,
            "probe=0 is not a probe"
        );
    }

    #[tokio::test]
    async fn a_probe_reports_an_event_the_project_would_drop() {
        let harness = harness_with("\n[capture]\nignore_paths = [\".env\"]\n");

        let payload = json!({
            "session_id": "session-probe",
            "hook_event_name": "PreToolUse",
            "cwd": harness.cwd.to_string_lossy(),
            "tool_name": "Read",
            "tool_input": {"file_path": harness.cwd.join(".env").to_string_lossy()},
        });
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/hook?agent=claude-code&probe=1")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request");

        let response = send(&harness.state, request).await;
        let report: ProbeReport = serde_json::from_str(&body_of(response).await).expect("report");
        assert!(!report.would_record);
        assert!(
            report.excluded.is_some_and(|path| path.ends_with(".env")),
            "a probe should name the rule that would drop the event"
        );
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

    /// The report `anamnesis status` prints comes from here, because here is
    /// the only place that knows. A client reading its own environment would
    /// confidently name a model this process was never started with.
    #[tokio::test]
    async fn whoami_says_how_this_server_compiles_memory() {
        let harness = harness();

        let response = send(
            &harness.state,
            with_token(HttpRequest::builder().uri("/whoami"), None),
        )
        .await;

        let body: serde_json::Value = serde_json::from_str(&body_of(response).await).expect("json");
        assert!(
            body.get("consolidation").is_some(),
            "the field has to be present even when there is no model, or a              client cannot tell 'counted' from 'an older server'"
        );
        assert_eq!(body["consolidation"], serde_json::Value::Null);
        assert_eq!(body["embedding"], serde_json::Value::Null);
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
                .peek_handoff(project, &anamnesis_core::handoff::Slot::shared())
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
                .peek_handoff(project, &anamnesis_core::handoff::Slot::shared())
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

    // ---------------------------------------------------------------
    // Per-operator slots. The gate is the interesting part: the same
    // session, recorded the same way, leaves its note in a different slot
    // depending on one line in the project's marker.
    // ---------------------------------------------------------------

    fn operator(name: &str) -> anamnesis_core::scope::OperatorName {
        anamnesis_core::scope::OperatorName::parse(name).expect("valid operator")
    }

    /// Work a session through to the handoff it leaves, attributed to `who`.
    fn session_by(harness: &Harness, who: &anamnesis_core::scope::OperatorName) {
        run_as(
            harness,
            "UserPromptSubmit",
            json!({"prompt": "do the thing"}),
            Some(who),
        );
        run_as(harness, "SessionEnd", json!({}), Some(who));
    }

    fn claim_as(
        harness: &Harness,
        session: &str,
        who: Option<&anamnesis_core::scope::OperatorName>,
    ) -> Option<String> {
        claim_handoff(
            &harness.state.store,
            &harness.cwd,
            &AgentKind::ClaudeCode,
            session,
            now(),
            who,
        )
        .expect("claim")
    }

    /// The failure this exists to stop: on a shared server, whoever starts
    /// next is handed someone else's context, and the person it was written
    /// for finds nothing waiting.
    #[test]
    fn with_per_user_slots_a_handoff_waits_for_the_operator_it_was_written_by() {
        let harness = harness_with("\n[slots]\nper_user = true\n");
        let alice = operator("alice");
        let bob = operator("bob");

        session_by(&harness, &alice);

        assert_eq!(
            claim_as(&harness, "bobs-session", Some(&bob)),
            None,
            "bob was handed alice's handoff"
        );

        let alices = claim_as(&harness, "alices-session", Some(&alice))
            .expect("alice's own handoff was waiting");
        assert!(alices.contains("do the thing"));
    }

    /// The gate. Without the setting a project keeps one slot, whatever the
    /// server can tell about who is calling — otherwise turning on
    /// authentication would quietly split one person's memory in two.
    #[test]
    fn without_the_setting_an_operator_does_not_split_the_slot() {
        let harness = harness();
        session_by(&harness, &operator("alice"));

        let claimed = claim_as(&harness, "bobs-session", Some(&operator("bob")))
            .expect("one slot, so the note is there to be claimed");
        assert!(claimed.contains("do the thing"));
    }

    /// A caller the server could not name is every anonymous caller, and they
    /// go on sharing the slot they have always shared.
    #[test]
    fn an_anonymous_caller_uses_the_shared_slot_even_where_slots_are_split() {
        let harness = harness_with("\n[slots]\nper_user = true\n");
        session_by(&harness, &operator("alice"));

        assert_eq!(
            claim_as(&harness, "anonymous-session", None),
            None,
            "an unnamed caller took a named operator's handoff"
        );
    }

    /// Provenance is not the setting: who ran a session is recorded whether or
    /// not the project separates slots, so turning the setting on can explain
    /// something about the sessions that came before it.
    #[test]
    fn a_session_records_its_operator_even_where_slots_are_shared() {
        let harness = harness();
        let ingested = run_as(
            &harness,
            "UserPromptSubmit",
            json!({"prompt": "do the thing"}),
            Some(&operator("alice")),
        );

        let session = harness
            .state
            .store
            .load_session(ingested.session_id)
            .expect("load")
            .expect("session exists");
        assert_eq!(session.operator, Some(operator("alice")));
    }
    /// A tool output of a few megabytes is an ordinary event: one `Read` of a
    /// large file makes one. What the server keeps of it is 16 KB, cut after
    /// parsing — so refusing the request outright would reject an event it was
    /// about to shorten anyway, and leave the hook holding a payload no retry
    /// can ever deliver.
    #[tokio::test]
    async fn an_oversized_tool_output_is_accepted_rather_than_refused() {
        let harness = harness();
        let payload = json!({
            "session_id": "session-huge",
            "hook_event_name": "PostToolUse",
            "cwd": harness.cwd.to_string_lossy(),
            "tool_name": "Read",
            "tool_input": {"file_path": "big.txt"},
            "tool_response": "x".repeat(3 * 1024 * 1024),
        });
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/hook?agent=claude-code")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request");

        let response = send(&harness.state, request).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    /// And the ceiling is still a ceiling. Raising the limit to fit a real
    /// payload is only defensible while there is a size past which the server
    /// stops reading: the body is buffered whole and scanned for secrets
    /// before a byte of it is kept.
    #[tokio::test]
    async fn a_body_past_the_ceiling_is_still_refused() {
        let harness = harness();
        let payload = json!({
            "session_id": "session-absurd",
            "hook_event_name": "PostToolUse",
            "cwd": harness.cwd.to_string_lossy(),
            "tool_name": "Read",
            "tool_response": "x".repeat(MAX_HOOK_BODY + 1),
        });
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/hook?agent=claude-code")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request");

        let response = send(&harness.state, request).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// One event, delivered twice, because that is what the queue does: a
    /// hook that gave up after a second on a server that was in fact
    /// recording keeps the event and offers it again later. Both arrivals
    /// carry the identity the sender minted, and the second one has to change
    /// nothing — a session that counts the same prompt twice is a session
    /// summarised wrongly, and nothing downstream can tell the copy apart.
    #[tokio::test]
    async fn an_event_delivered_twice_is_recorded_once() {
        let harness = harness();
        let event = "01998f3a-0000-7000-8000-00000000abcd";
        let body = json!({
            "session_id": "session-twice",
            "hook_event_name": "UserPromptSubmit",
            "cwd": harness.cwd.to_string_lossy(),
            "prompt": "do the thing",
        })
        .to_string();
        let request = || {
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/hook?agent=claude-code&event={event}"))
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .expect("request")
        };

        for _ in 0..2 {
            let response = send(&harness.state, request()).await;
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }

        let session = anamnesis_core::ids::SessionId::derive(project(&harness), "session-twice");
        let observations = harness
            .state
            .store
            .observations(session)
            .expect("observations");
        assert_eq!(
            observations.len(),
            1,
            "the replayed event was recorded a second time"
        );
    }

    /// The outage this server actually had, as an assertion. On 2026-09-01
    /// this repository's marker gained a `[sessions]` table hours before the
    /// installed server was rebuilt, and the older server answered `400` to
    /// every event of every session for three hours — nothing was wrong with
    /// the events, the file was simply newer than the binary reading it.
    /// Capture must survive a marker describing a feature this build does not
    /// have.
    #[tokio::test]
    async fn a_marker_written_for_a_newer_build_still_records() {
        let harness = harness_with("\n[a_feature_from_the_future]\nstale_after_minutes = 720\n");

        let response = send(&harness.state, hook_request(&harness, "SessionStart", None)).await;

        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "a table this build has no name for cost the whole session"
        );
    }

    /// And a sender that names nothing still has every event of its own. Two
    /// identical prompts in one session are two events, and collapsing them
    /// would lose one to a de-duplication nobody asked for.
    #[tokio::test]
    async fn events_without_an_identity_are_each_recorded() {
        let harness = harness();
        let body = json!({
            "session_id": "session-anon",
            "hook_event_name": "UserPromptSubmit",
            "cwd": harness.cwd.to_string_lossy(),
            "prompt": "again",
        })
        .to_string();

        for _ in 0..2 {
            let request = HttpRequest::builder()
                .method("POST")
                .uri("/hook?agent=claude-code")
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .expect("request");
            let response = send(&harness.state, request).await;
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }

        let session = anamnesis_core::ids::SessionId::derive(project(&harness), "session-anon");
        assert_eq!(
            harness
                .state
                .store
                .observations(session)
                .expect("observations")
                .len(),
            2
        );
    }
}
