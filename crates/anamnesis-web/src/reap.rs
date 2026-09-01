//! Summarising the sessions nobody closed.
//!
//! Consolidation hangs off one event. `SessionEnd` arrives, the session is
//! read, a page is written, a handoff is left. Every other path through the
//! system assumes that event comes.
//!
//! It does not always come. An editor that crashes sends nothing; a machine
//! that reboots sends nothing; a process someone kills sends nothing. The
//! session stays `open`, and because nothing ever revisits an open session,
//! its observations sit in the index and the transcript sits under `raw/` and
//! neither is ever read again. The work is not lost — `reindex` can still find
//! it — but nothing asks it to, so in practice the session is gone.
//!
//! This pass is the thing that revisits them. Three properties are deliberate.
//!
//! **It is late on purpose.** The threshold is hours, not minutes, because the
//! two states it has to tell apart look identical from here: a session that
//! died and a session whose operator went to lunch are both silent. Being
//! early would summarise the second one, and the cost of being late is only
//! that a page arrives later than it could have.
//!
//! **Reaping is not destructive.** A session that is summarised and then says
//! something else is reopened by the capture path, and ending again rewrites
//! the same page and supersedes its handoff. So the worst outcome of guessing
//! wrong is a page written twice, not a session cut in half.
//!
//! **A claim is a compare-and-swap.** The pass wakes on a timer and a model
//! call can outlast the gap between two ticks, so `open -> ending` has to be
//! atomic or two ticks will summarise the same session twice. That is what the
//! `ending` state is for.

use anamnesis_core::config::SessionsConfig;
use anamnesis_core::embedding::Embed;
use anamnesis_core::ids::SessionId;
use anamnesis_core::scope::{ResolvedScope, resolve_scope};
use anamnesis_store::OpenSession;
use jiff::Timestamp;

use crate::improve::TICK;
use crate::pipeline::{finalize, finalize_with_llm};
use crate::{AppState, WebError};

/// Why an open session was left where it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Left {
    /// It has been heard from recently enough to still be somebody's session.
    StillWarm,
    /// Its project has set `stale_after_minutes = 0`.
    Disabled,
    /// Its working copy no longer resolves to a project — moved, deleted, or
    /// on a drive that is not mounted. Nothing can be written for a session
    /// whose scope is unknown, because the scope is where the page would go.
    Unscopable,
    /// Another pass claimed it first.
    Claimed,
}

/// What one pass over the open sessions did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapReport {
    /// Sessions summarised, each with the page it produced. `None` means the
    /// session held nothing worth a page and was closed empty.
    pub reaped: Vec<(SessionId, Option<String>)>,
    /// Sessions that were due but could not be summarised. Each was put back
    /// so the next pass tries again.
    pub failed: Vec<SessionId>,
    /// Sessions deliberately left alone, and why.
    pub left: Vec<(SessionId, Left)>,
}

/// Whether a session has been silent long enough to act on.
fn overdue(session: &OpenSession, config: &SessionsConfig, now: Timestamp) -> bool {
    let silent_minutes =
        (now.as_millisecond() - session.last_seen.as_millisecond()) as f64 / 60_000.0;
    silent_minutes >= f64::from(config.stale_after_minutes)
}

/// Summarise one session the same way its own `SessionEnd` would have.
///
/// The two paths are the pipeline's, not this module's: a session closed by
/// the reaper and a session closed by its harness must produce the same page,
/// or the memory would carry two kinds of session and a reader would have to
/// know which they were looking at.
async fn close(
    state: &AppState,
    scope: &ResolvedScope,
    session_id: SessionId,
    now: Timestamp,
) -> Result<Option<String>, WebError> {
    match &state.llm {
        Some(settings) => {
            finalize_with_llm(
                &state.store,
                &state.wiki,
                scope,
                session_id,
                state.embedder.clone(),
                now,
                settings,
            )
            .await
        }
        // The same rule the handlers follow: summarising a session writes a
        // page, commits it, and embeds it, and none of that belongs on a
        // thread the server also answers requests with.
        None => {
            let store = state.store.clone();
            let wiki = state.wiki.clone();
            let embedder = state.embedder.clone();
            let scope = scope.clone();
            crate::off_runtime(move || {
                let held = wiki.lock();
                finalize(
                    &store,
                    &held,
                    &scope,
                    session_id,
                    embedder
                        .as_ref()
                        .map(|embedder| embedder.as_ref() as &dyn Embed),
                    now,
                )
            })
            .await
        }
    }
}

/// Summarise every open session that has gone quiet for long enough.
pub async fn reap(state: &AppState, now: Timestamp) -> ReapReport {
    let mut report = ReapReport::default();

    let open = match state.store.open_sessions() {
        Ok(open) => open,
        Err(error) => {
            tracing::error!(%error, "could not list the open sessions");
            return report;
        }
    };

    for session in open {
        let Ok(scope) = resolve_scope(&session.checkout_path) else {
            // Logged at debug: a checkout that has moved is somebody's normal
            // Tuesday, and this would otherwise be a warning on every tick for
            // the rest of the server's life.
            tracing::debug!(
                session = %session.id,
                path = %session.checkout_path.display(),
                "leaving an open session alone: its checkout no longer resolves"
            );
            report.left.push((session.id, Left::Unscopable));
            continue;
        };

        if scope.sessions.stale_after_minutes == 0 {
            report.left.push((session.id, Left::Disabled));
            continue;
        }
        if !overdue(&session, &scope.sessions, now) {
            report.left.push((session.id, Left::StillWarm));
            continue;
        }

        match state.store.begin_ending(session.id) {
            Ok(true) => {}
            Ok(false) => {
                report.left.push((session.id, Left::Claimed));
                continue;
            }
            Err(error) => {
                tracing::error!(%error, session = %session.id, "could not claim a session");
                report.failed.push(session.id);
                continue;
            }
        }

        match close(state, &scope, session.id, now).await {
            Ok(page) => {
                tracing::info!(
                    session = %session.id,
                    project = %scope.scope,
                    page = page.as_deref().unwrap_or("(empty)"),
                    "summarised a session nobody closed"
                );
                report.reaped.push((session.id, page));
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    session = %session.id,
                    "could not summarise an abandoned session"
                );
                // Back on offer. Left in `ending` it would be invisible to
                // every later pass, which is the one outcome worse than the
                // bug this whole module exists to fix.
                if let Err(error) = state.store.reopen_session(session.id) {
                    tracing::error!(
                        %error,
                        session = %session.id,
                        "could not reopen a session after a failed summary"
                    );
                }
                report.failed.push(session.id);
            }
        }
    }

    report
}

/// Sweep forever, starting immediately.
///
/// The first pass is not delayed, and the reason is the failure this exists
/// for: the most likely thing to have abandoned a session is the crash or the
/// reboot that the server is now starting up from.
pub async fn run_reaper(state: AppState) {
    loop {
        reap(&state, Timestamp::now()).await;
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::observation::{BoundedBody, EventKind};
    use anamnesis_core::session::AgentKind;
    use anamnesis_store::{RawSpool, Store, new_observation, new_session};
    use anamnesis_wiki::Wiki;

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        state: AppState,
        scope: ResolvedScope,
    }

    /// A project whose marker carries `extra` beyond the scope table.
    fn harness_with(extra: &str) -> Harness {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            format!("[scope]\nworkspace = \"default\"\nproject = \"widget\"\n{extra}"),
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");

        let data = tempfile::tempdir().expect("data");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        store.upsert_project(&scope, now()).expect("project");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");

        Harness {
            state: AppState::new(store, wiki)
                .with_raw(Some(RawSpool::new(data.path().join("raw")))),
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn harness() -> Harness {
        harness_with("")
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    fn minutes_ago(minutes: i64) -> Timestamp {
        now() - jiff::Span::new().minutes(minutes)
    }

    /// An open session whose newest observation is `minutes` old.
    ///
    /// It carries a prompt because a session of nothing but lifecycle
    /// boundaries is summarised to nothing on purpose, and most of these tests
    /// are about the page.
    fn session_silent_for(harness: &Harness, name: &str, minutes: i64) -> SessionId {
        let id = SessionId::derive(harness.scope.project_id, name);
        let session = new_session(
            id,
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            harness.scope.root.clone(),
            minutes_ago(minutes + 5),
            None,
        );
        harness
            .state
            .store
            .ensure_session(&session)
            .expect("session");
        harness
            .state
            .store
            .insert_observation(&new_observation(
                id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating("make the tests pass", 1024),
                minutes_ago(minutes),
            ))
            .expect("observation");
        id
    }

    fn state_of(harness: &Harness, id: SessionId) -> String {
        harness
            .state
            .store
            .recent_sessions(harness.scope.project_id, 10)
            .expect("list")
            .into_iter()
            .find(|row| row.id == id)
            .expect("session")
            .state
    }

    #[tokio::test]
    async fn a_session_nobody_closed_is_summarised_and_gets_a_page() {
        let harness = harness();
        let id = session_silent_for(&harness, "abandoned", 60 * 24);

        let report = reap(&harness.state, now()).await;

        assert_eq!(report.reaped.len(), 1, "{report:?}");
        assert_eq!(report.reaped[0].0, id);
        assert!(report.reaped[0].1.is_some(), "it should have left a page");
        assert_eq!(state_of(&harness, id), "closed");
    }

    /// The whole reason the default is measured in hours.
    #[tokio::test]
    async fn a_session_that_is_merely_quiet_is_left_alone() {
        let harness = harness();
        let id = session_silent_for(&harness, "at-lunch", 45);

        let report = reap(&harness.state, now()).await;

        assert!(report.reaped.is_empty(), "{report:?}");
        assert_eq!(report.left, vec![(id, Left::StillWarm)]);
        assert_eq!(state_of(&harness, id), "open");
    }

    #[tokio::test]
    async fn a_project_can_turn_it_off() {
        let harness = harness_with("[sessions]\nstale_after_minutes = 0\n");
        let id = session_silent_for(&harness, "abandoned", 60 * 24 * 30);

        let report = reap(&harness.state, now()).await;

        assert!(report.reaped.is_empty(), "{report:?}");
        assert_eq!(report.left, vec![(id, Left::Disabled)]);
        assert_eq!(state_of(&harness, id), "open");
    }

    #[tokio::test]
    async fn a_project_can_ask_to_be_swept_sooner() {
        let harness = harness_with("[sessions]\nstale_after_minutes = 30\n");
        let id = session_silent_for(&harness, "abandoned", 45);

        let report = reap(&harness.state, now()).await;

        assert_eq!(report.reaped.len(), 1, "{report:?}");
        assert_eq!(report.reaped[0].0, id);
    }

    /// A session of nothing but boundaries earns no page, and a wiki full of
    /// empty stubs would make every later search worse — but it must still
    /// stop being open, or every pass forever will pick it up again.
    #[tokio::test]
    async fn an_empty_session_is_closed_without_a_page() {
        let harness = harness();
        let id = SessionId::derive(harness.scope.project_id, "empty");
        let session = new_session(
            id,
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            harness.scope.root.clone(),
            minutes_ago(60 * 24),
            None,
        );
        harness
            .state
            .store
            .ensure_session(&session)
            .expect("session");

        let report = reap(&harness.state, now()).await;

        assert_eq!(report.reaped, vec![(id, None)]);
        assert_eq!(state_of(&harness, id), "closed");
    }

    /// A pass that has already claimed a session must not have it taken from
    /// under it by the next tick.
    #[tokio::test]
    async fn a_claimed_session_is_not_claimed_twice() {
        let harness = harness();
        let id = session_silent_for(&harness, "abandoned", 60 * 24);
        assert!(harness.state.store.begin_ending(id).expect("claim"));

        let report = reap(&harness.state, now()).await;

        assert!(report.reaped.is_empty(), "{report:?}");
        assert!(report.left.is_empty(), "an ending session is not on offer");
    }

    /// Running twice over the same memory must not write the page twice.
    #[tokio::test]
    async fn a_second_pass_finds_nothing_left_to_do() {
        let harness = harness();
        session_silent_for(&harness, "abandoned", 60 * 24);
        reap(&harness.state, now()).await;

        let second = reap(&harness.state, now()).await;

        assert_eq!(second, ReapReport::default());
    }

    /// A session whose checkout does not resolve to a project cannot be given
    /// a page, because the scope is where the page would go. The realistic
    /// cause is a marker somebody has just broken: resolution reads it, and a
    /// file that is not TOML fails the whole scope rather than being guessed
    /// at. The session is left alone, so fixing the marker is enough to get
    /// the page — nothing had to be recovered in between.
    #[tokio::test]
    async fn a_session_whose_scope_cannot_be_resolved_is_left_where_it_is() {
        let harness = harness();
        let broken = tempfile::tempdir().expect("dir");
        std::fs::write(
            broken.path().join(".anamnesis.toml"),
            "[scope
workspace =",
        )
        .expect("marker");
        assert!(
            resolve_scope(broken.path()).is_err(),
            "the marker should be unreadable, or this test proves nothing"
        );

        let id = SessionId::derive(harness.scope.project_id, "broken-marker");
        let session = new_session(
            id,
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            broken.path().to_path_buf(),
            minutes_ago(60 * 24),
            None,
        );
        harness
            .state
            .store
            .ensure_session(&session)
            .expect("session");

        let report = reap(&harness.state, now()).await;

        assert_eq!(report.left, vec![(id, Left::Unscopable)]);
        assert!(report.reaped.is_empty(), "{report:?}");
        assert_eq!(state_of(&harness, id), "open");
    }

    /// Two sessions, one due and one not: the pass must not be all-or-nothing.
    #[tokio::test]
    async fn one_session_being_warm_does_not_hold_up_another() {
        let harness = harness();
        let warm = session_silent_for(&harness, "at-lunch", 10);
        let cold = session_silent_for(&harness, "abandoned", 60 * 24);

        let report = reap(&harness.state, now()).await;

        assert_eq!(report.reaped.len(), 1, "{report:?}");
        assert_eq!(report.reaped[0].0, cold);
        assert_eq!(report.left, vec![(warm, Left::StillWarm)]);
    }

    /// A project that never registered has no rows to reap, and the pass must
    /// not mind.
    #[tokio::test]
    async fn a_memory_with_no_open_sessions_is_a_quiet_pass() {
        let harness = harness();

        assert_eq!(reap(&harness.state, now()).await, ReapReport::default());
    }

    #[test]
    fn the_default_threshold_is_measured_in_hours_not_minutes() {
        // Guards the number itself: the difference between the default and
        // something merely quiet is the whole safety argument for this module.
        assert!(SessionsConfig::default().stale_after_minutes >= 60 * 4);
    }

    #[test]
    fn a_session_is_overdue_only_once_the_threshold_has_passed() {
        let config = SessionsConfig {
            stale_after_minutes: 30,
        };
        let at = |minutes| OpenSession {
            id: SessionId::new(),
            checkout_path: std::path::PathBuf::from("/repo"),
            last_seen: minutes_ago(minutes),
        };

        assert!(!overdue(&at(29), &config, now()));
        assert!(overdue(&at(30), &config, now()), "the boundary counts");
        assert!(overdue(&at(31), &config, now()));
    }
}
