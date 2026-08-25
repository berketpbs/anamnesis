//! Auto-improve: one pass over a project's memory, and the schedule that
//! runs it.
//!
//! The rules live in [`anamnesis_core::improve`] and the rows live in
//! `anamnesis_store`. What is here is the part that touches both plus the
//! wiki: filing what a pass noticed, carrying out the proposals a project has
//! said may be carried out, and — because this crate is the only thing in the
//! system that runs for longer than one command — the loop that decides when
//! that happens.
//!
//! Three properties are deliberate.
//!
//! **Approval is the default.** `[auto_improve] require_approval` starts true,
//! so a pass files proposals and stops. Only a project that has said otherwise
//! has its pages changed by a schedule, and even then only for the one kind of
//! proposal that can be carried out mechanically.
//!
//! **The schedule is the project's, not the server's.** One server serves
//! every project that talks to it, and each keeps its own `[auto_improve]`
//! table. So the loop ticks on a fixed short interval and asks each project
//! whether *its* interval has elapsed since *its* last pass, which is also why
//! the last pass is recorded in the index rather than in memory: restarting
//! the server must not restart everyone's clock.
//!
//! **A pass survives its own failures.** One page that cannot be read must not
//! stop the other proposals, and must not stop the next project. Failures are
//! collected into the report and logged, never propagated out of the loop.

use std::time::Duration;

use anamnesis_core::config::{AutoImproveConfig, MarkerConfig};
use anamnesis_core::ids::ProjectId;
use anamnesis_core::improve::{ProposalKind, ProposalState, propose};
use anamnesis_core::page::{Page, PagePath, Tier};
use anamnesis_core::scope::{Scope, find_marker};
use anamnesis_store::{Filed, ProjectRow, Store, StoredProposal};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

use crate::{AppState, WebError};

/// How often the scheduler wakes up to ask who is due.
///
/// Shorter than any interval anyone would configure, so that a project asking
/// for sixty minutes gets something close to sixty minutes rather than the
/// next multiple of a long sleep.
pub const TICK: Duration = Duration::from_secs(60);

/// What applying one proposal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The page was promoted, in the commit named here.
    Promoted {
        /// Commit recording the change.
        commit: String,
    },
    /// The page had already been promoted, by hand or by an earlier pass.
    /// The proposal is resolved rather than applied: the memory improved,
    /// but not because of this.
    AlreadyDurable,
    /// Nothing mechanical can carry this out; it is waiting for a person.
    NeedsAPerson,
}

/// One proposal that a pass carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    /// What the proposal was about.
    pub subject: String,
    /// What happened to it.
    pub outcome: Outcome,
}

/// What one pass over one project did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PassReport {
    /// Proposals filed, refreshed, and resolved.
    pub filed: Filed,
    /// Proposals carried out, when the project allows that.
    pub carried: Vec<Carried>,
    /// Proposals that could not be carried out, and why.
    pub failures: Vec<(String, String)>,
    /// Proposals still waiting after the pass.
    pub open: usize,
}

/// Run one improvement pass over one project.
///
/// Returns `None` when the project has turned auto-improve off — the caller
/// then knows the difference between "nothing to propose" and "not asked to
/// look", which a report of zeroes cannot say.
pub fn run_pass(
    store: &Store,
    wiki: &Wiki,
    scope: &Scope,
    project_id: ProjectId,
    config: &AutoImproveConfig,
    now: Timestamp,
) -> Result<Option<PassReport>, WebError> {
    if !config.enabled {
        return Ok(None);
    }

    let facts = store.improve_facts(project_id)?;
    let proposals = propose(&facts, now);
    let filed = store.record_proposals(project_id, &proposals, now)?;

    let mut report = PassReport {
        filed,
        ..PassReport::default()
    };

    if !config.require_approval {
        // Every open applicable proposal, not only the ones filed just now: a
        // project that switched approval off is saying these may be carried
        // out, including the ones that were waiting for that answer.
        for proposal in store.proposals(project_id, true)? {
            if !proposal.kind.is_applicable() {
                continue;
            }
            match apply(store, wiki, scope, project_id, &proposal, now) {
                Ok(outcome) => report.carried.push(Carried {
                    subject: proposal.subject.clone(),
                    outcome,
                }),
                // One page that cannot be read is not a reason to abandon the
                // rest of the pass.
                Err(error) => report
                    .failures
                    .push((proposal.subject.clone(), error.to_string())),
            }
        }
    }

    report.open = store.proposals(project_id, true)?.len();
    Ok(Some(report))
}

/// Carry out one proposal.
///
/// The wiki is written before the index, the same order every other write in
/// this system uses: the markdown is the source of truth, and an index row
/// that disagrees with it is what `anamnesis reindex` exists to repair.
pub fn apply(
    store: &Store,
    wiki: &Wiki,
    scope: &Scope,
    project_id: ProjectId,
    proposal: &StoredProposal,
    now: Timestamp,
) -> Result<Outcome, WebError> {
    match proposal.kind {
        ProposalKind::PromoteTier => promote(store, wiki, scope, project_id, proposal, now),
        ProposalKind::WriteMissingPage => Ok(Outcome::NeedsAPerson),
    }
}

/// Move a page into the semantic tier, where the sweep cannot reach it.
fn promote(
    store: &Store,
    wiki: &Wiki,
    scope: &Scope,
    project_id: ProjectId,
    proposal: &StoredProposal,
    now: Timestamp,
) -> Result<Outcome, WebError> {
    let path = PagePath::parse(&proposal.subject)?;

    // Read from the wiki rather than from the index: the page may have been
    // edited by hand since the proposal was filed, and the promotion must
    // keep whatever it says now.
    let parsed = wiki.read_page(scope, &path)?;
    if parsed.frontmatter.tier.is_durable() {
        store.decide_proposal(proposal.id, ProposalState::Resolved, now)?;
        return Ok(Outcome::AlreadyDurable);
    }

    let mut frontmatter = parsed.frontmatter;
    frontmatter.tier = Tier::Semantic;
    let entities = frontmatter.entities.clone();
    let mut page = Page::new(project_id, path.clone(), frontmatter, parsed.body);

    let commit = wiki.write_page(
        scope,
        &page,
        &format!("improve: promote {path} to the semantic tier"),
    )?;
    page.git_commit = Some(commit.clone());

    store.upsert_page(&page, now)?;
    store.set_page_entities(project_id, page.id, &entities)?;
    store.set_page_links(
        project_id,
        page.id,
        &anamnesis_wiki::extract_links(&page.body),
    )?;
    store.decide_proposal(proposal.id, ProposalState::Applied, now)?;

    Ok(Outcome::Promoted { commit })
}

/// Why a project was not improved on this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skipped {
    /// The index does not know where the project's working copy is, so its
    /// settings cannot be read. Registering it again from that directory —
    /// any hook, or any CLI command run there — records it.
    RootUnknown,
    /// No marker file at that location, so the project keeps the defaults,
    /// and the default schedule is off.
    NoMarker,
    /// The marker exists but does not parse. Reported rather than treated as
    /// absent: a typo in a file that governs what a schedule may change to a
    /// project's memory should be visible.
    MarkerUnreadable,
    /// `[auto_improve] enabled = false`.
    Disabled,
    /// `[auto_improve.scheduler] enabled = false`, which is the default.
    NotScheduled,
    /// Its interval has not elapsed yet.
    NotDue,
}

impl Skipped {
    /// One line naming the reason, for a log or a status line.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RootUnknown => "no recorded working copy",
            Self::NoMarker => "no marker file",
            Self::MarkerUnreadable => "marker file does not parse",
            Self::Disabled => "auto-improve disabled",
            Self::NotScheduled => "no schedule configured",
            Self::NotDue => "not due yet",
        }
    }
}

/// What one tick of the scheduler did across every project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickReport {
    /// Projects that were improved, with what their pass did.
    pub passes: Vec<(Scope, PassReport)>,
    /// Projects that were not, and why.
    pub skipped: Vec<(Scope, Skipped)>,
}

/// Decide whether a project is due, reading the settings it keeps itself.
///
/// Split out from the tick so the decision can be tested without a wiki, a
/// clock, or a running server.
pub fn due(
    project: &ProjectRow,
    now: Timestamp,
) -> std::result::Result<AutoImproveConfig, Skipped> {
    let Some(root) = &project.root else {
        return Err(Skipped::RootUnknown);
    };
    let Some(marker) = find_marker(root) else {
        return Err(Skipped::NoMarker);
    };
    let Ok(config) = MarkerConfig::load(&marker.path) else {
        return Err(Skipped::MarkerUnreadable);
    };

    let auto = config.auto_improve;
    if !auto.enabled {
        return Err(Skipped::Disabled);
    }
    if !auto.scheduler.enabled {
        return Err(Skipped::NotScheduled);
    }

    match project.improved_at {
        // Never improved: due now, whatever the interval. The alternative is
        // a project that waits an hour to be looked at for the first time.
        None => Ok(auto),
        Some(last) => {
            let elapsed_minutes = (now.as_millisecond() - last.as_millisecond()) as f64 / 60_000.0;
            if elapsed_minutes >= f64::from(auto.scheduler.interval_minutes) {
                Ok(auto)
            } else {
                Err(Skipped::NotDue)
            }
        }
    }
}

/// Improve every project whose interval has elapsed.
///
/// Synchronous, and deliberately so: it is SQLite and git work, and it runs
/// on a blocking thread rather than on the runtime that has to answer hooks
/// within a second.
pub fn tick(state: &AppState, now: Timestamp) -> TickReport {
    let mut report = TickReport::default();

    let projects = match state.store.projects() {
        Ok(projects) => projects,
        Err(error) => {
            tracing::error!(%error, "auto-improve could not list projects");
            return report;
        }
    };

    for project in projects {
        let config = match due(&project, now) {
            Ok(config) => config,
            Err(reason) => {
                report.skipped.push((project.scope.clone(), reason));
                continue;
            }
        };

        let wiki = state.wiki.lock();
        let pass = run_pass(
            &state.store,
            &wiki,
            &project.scope,
            project.project_id,
            &config,
            now,
        );
        drop(wiki);

        match pass {
            Ok(Some(pass)) => {
                if let Err(error) = state.store.mark_improved(project.project_id, now) {
                    // The pass happened; only the record of when did not. Left
                    // unmarked it runs again next tick, which is wasteful and
                    // harmless — unlike claiming a pass that never ran.
                    tracing::warn!(%error, project = %project.scope, "could not record the pass");
                }
                tracing::info!(
                    project = %project.scope,
                    filed = pass.filed.filed,
                    refreshed = pass.filed.refreshed,
                    resolved = pass.filed.resolved,
                    carried = pass.carried.len(),
                    open = pass.open,
                    "auto-improve pass"
                );
                for (subject, error) in &pass.failures {
                    tracing::warn!(project = %project.scope, subject, error, "proposal failed");
                }
                report.passes.push((project.scope, pass));
            }
            // `due` already refused disabled projects; reaching here means the
            // marker changed between the two reads.
            Ok(None) => report.skipped.push((project.scope, Skipped::Disabled)),
            Err(error) => {
                tracing::error!(%error, project = %project.scope, "auto-improve pass failed");
            }
        }
    }

    report
}

/// Tick forever, starting immediately.
///
/// The first tick is not delayed: with the last pass recorded per project,
/// starting the server improves only what was already due, and waiting a full
/// interval to discover that would make a restart cost a cycle.
pub async fn run_scheduler(state: AppState) {
    loop {
        let ticking = state.clone();
        // Blocking work belongs on a blocking thread: a git commit in the
        // middle of the runtime would delay the hooks it shares it with.
        if let Err(error) =
            tokio::task::spawn_blocking(move || tick(&ticking, Timestamp::now())).await
        {
            tracing::error!(%error, "auto-improve tick panicked");
        }
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::page::{Frontmatter, PageStatus};
    use anamnesis_core::scope::{ResolvedScope, resolve_scope};
    use anamnesis_core::sweep::{PageFacts, SweepPolicy, Verdict, judge};
    use anamnesis_store::RawSpool;

    struct Harness {
        repo: tempfile::TempDir,
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
            repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    fn days_ago(days: i64) -> Timestamp {
        now() - jiff::Span::new().hours(days * 24)
    }

    /// Write a page to wiki and index as it stood `age` days ago, read `reads` times.
    fn page(harness: &Harness, path: &str, age: i64, reads: u32) -> Page {
        let mut frontmatter = Frontmatter::new("A page", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Episodic;
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            frontmatter,
            "Body about sqlite.",
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
            .upsert_page(&page, days_ago(age))
            .expect("upsert");
        for _ in 0..reads {
            harness
                .state
                .store
                .record_access(page.id, days_ago(1))
                .expect("access");
        }
        page
    }

    fn pass(harness: &Harness, config: &AutoImproveConfig) -> Option<PassReport> {
        let wiki = harness.state.wiki.lock();
        run_pass(
            &harness.state.store,
            &wiki,
            &harness.scope.scope,
            harness.scope.project_id,
            config,
            now(),
        )
        .expect("pass")
    }

    fn approving() -> AutoImproveConfig {
        AutoImproveConfig {
            require_approval: false,
            ..AutoImproveConfig::default()
        }
    }

    fn tier_on_disk(harness: &Harness, page: &Page) -> Tier {
        harness
            .state
            .wiki
            .lock()
            .read_page(&harness.scope.scope, &page.path)
            .expect("read")
            .frontmatter
            .tier
    }

    #[test]
    fn a_page_several_sessions_came_back_to_is_proposed() {
        let harness = harness_with("");
        page(&harness, "sessions/read.md", 40, 4);
        page(&harness, "sessions/quiet.md", 40, 0);

        let report = pass(&harness, &AutoImproveConfig::default()).expect("enabled");
        assert_eq!(report.filed.filed, 1);
        assert_eq!(report.open, 1);

        let open = harness
            .state
            .store
            .proposals(harness.scope.project_id, true)
            .expect("list");
        assert_eq!(open[0].kind, ProposalKind::PromoteTier);
        assert_eq!(open[0].subject, "sessions/read.md");
    }

    #[test]
    fn a_pass_that_needs_approval_changes_nothing() {
        let harness = harness_with("");
        let read = page(&harness, "sessions/read.md", 40, 4);
        let commits = harness.state.wiki.lock().commit_count().expect("count");

        let report = pass(&harness, &AutoImproveConfig::default()).expect("enabled");
        assert!(report.carried.is_empty());
        assert_eq!(tier_on_disk(&harness, &read), Tier::Episodic);
        assert_eq!(
            harness.state.wiki.lock().commit_count().expect("count"),
            commits,
            "nothing was written"
        );
    }

    #[test]
    fn a_project_that_allows_it_has_the_page_promoted() {
        let harness = harness_with("");
        let read = page(&harness, "sessions/read.md", 40, 4);

        let report = pass(&harness, &approving()).expect("enabled");
        assert_eq!(report.carried.len(), 1);
        assert!(matches!(
            report.carried[0].outcome,
            Outcome::Promoted { .. }
        ));
        assert_eq!(report.open, 0);

        assert_eq!(tier_on_disk(&harness, &read), Tier::Semantic);
        let indexed = harness
            .state
            .store
            .improve_facts(harness.scope.project_id)
            .expect("facts");
        let stats = indexed
            .pages
            .iter()
            .find(|p| p.page_id == read.id)
            .expect("page");
        assert_eq!(stats.tier, Tier::Semantic, "the index agrees with the wiki");

        let decided = harness
            .state
            .store
            .proposals(harness.scope.project_id, false)
            .expect("list");
        assert_eq!(decided[0].state, ProposalState::Applied);
    }

    #[test]
    fn a_promoted_page_is_out_of_the_sweeps_reach() {
        // The point of promotion, and the reason it needs approval by default:
        // a durable page is exempt from the decay sweep. This is how a page
        // earns that instead of someone remembering to pin it.
        //
        // The contrast only shows later. What makes a page promotable — being
        // read recently — is also what saves it from the sweep today, so the
        // question promotion actually answers is what happens a year after
        // the reads stop.
        let harness = harness_with("");
        let read = page(&harness, "sessions/read.md", 40, 4);
        let a_year_on = now() + jiff::Span::new().hours(365 * 24);

        let facts = |harness: &Harness| -> PageFacts {
            harness
                .state
                .store
                .sweep_rows(harness.scope.project_id)
                .expect("rows")
                .into_iter()
                .find(|row| row.page_id == read.id)
                .expect("row")
                .facts
        };

        assert!(
            judge(&facts(&harness), SweepPolicy::default(), a_year_on).forgets(),
            "unpromoted, a year of silence is enough to forget it"
        );

        pass(&harness, &approving()).expect("enabled");

        assert!(matches!(
            judge(&facts(&harness), SweepPolicy::default(), a_year_on),
            Verdict::Exempt { .. }
        ));
    }

    #[test]
    fn promotion_keeps_what_someone_wrote_since_the_proposal() {
        let harness = harness_with("");
        let original = page(&harness, "sessions/read.md", 40, 4);
        pass(&harness, &AutoImproveConfig::default()).expect("enabled");

        // Edited by hand between the proposal and the decision.
        let mut edited = original.clone();
        edited.body = "Rewritten by a person who knew more.".to_owned();
        harness
            .state
            .wiki
            .lock()
            .write_page(&harness.scope.scope, &edited, "edit")
            .expect("write");

        pass(&harness, &approving()).expect("enabled");

        let read = harness
            .state
            .wiki
            .lock()
            .read_page(&harness.scope.scope, &original.path)
            .expect("read");
        assert_eq!(read.frontmatter.tier, Tier::Semantic);
        assert_eq!(read.body.trim(), "Rewritten by a person who knew more.");
    }

    #[test]
    fn a_page_promoted_by_hand_resolves_the_proposal_rather_than_claiming_it() {
        let harness = harness_with("");
        let read = page(&harness, "sessions/read.md", 40, 4);
        pass(&harness, &AutoImproveConfig::default()).expect("enabled");

        let mut promoted = read.clone();
        promoted.frontmatter.tier = Tier::Semantic;
        harness
            .state
            .wiki
            .lock()
            .write_page(&harness.scope.scope, &promoted, "promote by hand")
            .expect("write");

        let proposal = harness
            .state
            .store
            .proposals(harness.scope.project_id, true)
            .expect("list")
            .remove(0);
        let wiki = harness.state.wiki.lock();
        let outcome = apply(
            &harness.state.store,
            &wiki,
            &harness.scope.scope,
            harness.scope.project_id,
            &proposal,
            now(),
        )
        .expect("apply");
        drop(wiki);

        assert_eq!(outcome, Outcome::AlreadyDurable);
        assert_eq!(
            harness
                .state
                .store
                .proposals(harness.scope.project_id, false)
                .expect("list")[0]
                .state,
            ProposalState::Resolved
        );
    }

    #[test]
    fn a_missing_page_proposal_waits_for_a_person() {
        let harness = harness_with("");
        let first = page(&harness, "sessions/a.md", 1, 0);
        let second = page(&harness, "sessions/b.md", 1, 0);
        for source in [&first, &second] {
            harness
                .state
                .store
                .set_page_links(
                    harness.scope.project_id,
                    source.id,
                    &["gotchas/windows-bom.md".to_owned()],
                )
                .expect("links");
        }

        // Even with approval switched off, nothing can invent the page.
        let report = pass(&harness, &approving()).expect("enabled");
        assert_eq!(report.filed.filed, 1);
        assert!(
            report.carried.is_empty(),
            "not applicable, so not attempted"
        );
        assert_eq!(report.open, 1);
    }

    #[test]
    fn one_unreadable_page_does_not_stop_the_pass() {
        let harness = harness_with("");
        let broken = page(&harness, "sessions/broken.md", 40, 4);
        let fine = page(&harness, "sessions/fine.md", 40, 4);
        std::fs::remove_file(
            harness
                .state
                .wiki
                .lock()
                .locate(&harness.scope.scope, &broken.path),
        )
        .expect("remove");

        let report = pass(&harness, &approving()).expect("enabled");
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, "sessions/broken.md");
        assert_eq!(report.carried.len(), 1, "the other page was still promoted");
        assert_eq!(tier_on_disk(&harness, &fine), Tier::Semantic);
    }

    #[test]
    fn a_project_that_turned_it_off_is_not_looked_at() {
        let harness = harness_with("");
        page(&harness, "sessions/read.md", 40, 4);

        let off = AutoImproveConfig {
            enabled: false,
            ..AutoImproveConfig::default()
        };
        assert!(
            pass(&harness, &off).is_none(),
            "not asked to look is not the same as nothing to propose"
        );
        assert!(
            harness
                .state
                .store
                .proposals(harness.scope.project_id, false)
                .expect("list")
                .is_empty()
        );
    }

    /// The project row a scheduler would read, as the index holds it.
    fn project_row(harness: &Harness) -> ProjectRow {
        harness.state.store.projects().expect("projects").remove(0)
    }

    #[test]
    fn the_default_marker_asks_for_no_schedule() {
        let harness = harness_with("");
        assert_eq!(
            due(&project_row(&harness), now()),
            Err(Skipped::NotScheduled)
        );
    }

    #[test]
    fn a_scheduled_project_that_has_never_run_is_due_now() {
        let harness =
            harness_with("[auto_improve.scheduler]\nenabled = true\ninterval_minutes = 60\n");
        assert!(due(&project_row(&harness), now()).is_ok());
    }

    #[test]
    fn an_interval_is_measured_from_the_last_pass() {
        let harness =
            harness_with("[auto_improve.scheduler]\nenabled = true\ninterval_minutes = 60\n");
        harness
            .state
            .store
            .mark_improved(harness.scope.project_id, now())
            .expect("mark");

        let row = project_row(&harness);
        let minutes = |m: i64| now() + jiff::Span::new().minutes(m);
        assert_eq!(due(&row, minutes(59)), Err(Skipped::NotDue));
        assert!(due(&row, minutes(60)).is_ok());
    }

    #[test]
    fn a_project_that_disabled_auto_improve_is_never_due() {
        let harness = harness_with(
            "[auto_improve]\nenabled = false\n\n[auto_improve.scheduler]\nenabled = true\ninterval_minutes = 1\n",
        );
        assert_eq!(due(&project_row(&harness), now()), Err(Skipped::Disabled));
    }

    #[test]
    fn a_marker_that_does_not_parse_is_reported_not_ignored() {
        let harness = harness_with("");
        std::fs::write(
            harness.repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n[auto_improve]\nenabled = \"yes\"\n",
        )
        .expect("marker");
        assert_eq!(
            due(&project_row(&harness), now()),
            Err(Skipped::MarkerUnreadable)
        );
    }

    #[test]
    fn a_project_whose_working_copy_is_unknown_cannot_be_scheduled() {
        let harness = harness_with("");
        let mut row = project_row(&harness);
        row.root = None;
        assert_eq!(due(&row, now()), Err(Skipped::RootUnknown));

        // A directory with no marker anywhere above it. Not a subdirectory of
        // the repository: `find_marker` walks ancestors, so one of those would
        // still find the project it was moved out of.
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        row.root = Some(elsewhere.path().to_path_buf());
        assert_eq!(due(&row, now()), Err(Skipped::NoMarker));
    }

    #[test]
    fn a_tick_improves_what_is_due_and_says_why_it_skipped_the_rest() {
        let harness = harness_with(
            "[auto_improve]\nenabled = true\nrequire_approval = false\n\n[auto_improve.scheduler]\nenabled = true\ninterval_minutes = 60\n",
        );
        let read = page(&harness, "sessions/read.md", 40, 4);

        let report = tick(&harness.state, now());
        assert_eq!(report.passes.len(), 1);
        assert_eq!(report.passes[0].1.carried.len(), 1);
        assert_eq!(tier_on_disk(&harness, &read), Tier::Semantic);

        // And the interval now holds it back.
        let second = tick(&harness.state, now());
        assert!(second.passes.is_empty());
        assert_eq!(second.skipped[0].1, Skipped::NotDue);
    }

    #[test]
    fn a_pass_leaves_pages_it_has_no_opinion_about_alone() {
        let harness = harness_with("");
        let untouched = page(&harness, "sessions/quiet.md", 400, 0);
        let mut wrong = page(&harness, "sessions/wrong.md", 40, 9);
        wrong.frontmatter.status = PageStatus::DoNotAnswerFrom;
        harness
            .state
            .wiki
            .lock()
            .write_page(&harness.scope.scope, &wrong, "mark wrong")
            .expect("write");
        harness
            .state
            .store
            .upsert_page(&wrong, days_ago(40))
            .expect("upsert");

        let report = pass(&harness, &approving()).expect("enabled");
        assert_eq!(report.filed.filed, 0);
        assert_eq!(tier_on_disk(&harness, &untouched), Tier::Episodic);
        assert_eq!(tier_on_disk(&harness, &wrong), Tier::Episodic);
    }
}
