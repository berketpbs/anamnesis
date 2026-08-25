//! Forgetting: deciding what a project's memory should let go of, and
//! letting go of it.
//!
//! Three layers meet here and none of them overlap. [`anamnesis_core::sweep`]
//! decides — purely, from numbers and flags — what happens to one page.
//! [`anamnesis_store`] supplies those facts and drops index rows.
//! [`anamnesis_wiki`] removes the markdown and records the removal as one
//! commit. This module is the seam: it judges every page, ranks the verdicts
//! into something a person can read, and — only when asked — carries them out
//! in the order that survives being interrupted.
//!
//! Two properties are deliberate.
//!
//! **Reporting is the default and deleting is the exception.** [`plan`]
//! changes nothing; [`apply`] is reached only from `--apply`. A retention
//! policy is a guess about what will matter later, and the first run of one
//! against a real wiki is where the guess gets corrected.
//!
//! **The index goes before the wiki.** Interrupted between the two, a sweep
//! leaves pages that are briefly unfindable and that `anamnesis reindex`
//! restores in full. The other order leaves index rows pointing at markdown
//! that no longer exists, which no rebuild repairs.

use anamnesis_core::ids::ProjectId;
use anamnesis_core::page::PagePath;
use anamnesis_core::scope::ResolvedScope;
use anamnesis_core::sweep::{Exemption, SweepPolicy, Verdict, judge};
use anamnesis_store::{Store, SweepRow};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

/// One page and what the policy decided about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    /// The page, as the index holds it.
    pub row: SweepRow,
    /// What the policy decided.
    pub verdict: Verdict,
}

impl Judged {
    /// Why this page is going, in the words a report uses.
    pub fn reason(&self, now: Timestamp) -> String {
        match self.verdict {
            Verdict::Expired => match self.row.facts.expires_at {
                Some(deadline) => format!("expired {}", deadline.strftime("%Y-%m-%d")),
                None => "expired".to_owned(),
            },
            Verdict::Forget { score } | Verdict::Keep { score } => {
                let inputs = self.row.facts.inputs(now);
                let age = inputs.age_days.max(0.0).round() as i64;
                match inputs.access_count {
                    0 => format!("score {score:.3}, {age}d old, never read"),
                    reads => format!(
                        "score {score:.3}, {age}d old, read {reads}× {}d ago",
                        inputs.days_since_access.max(0.0).round() as i64
                    ),
                }
            }
            Verdict::Exempt { exemption, .. } => exemption.as_str().to_owned(),
        }
    }
}

/// What a sweep would do to one project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// Pages the policy condemned, most forgettable first.
    pub forget: Vec<Judged>,
    /// Pages the score saved, in the same order.
    pub keep: Vec<Judged>,
    /// Pages a rule put out of reach.
    pub exempt: Vec<Judged>,
}

impl Plan {
    /// How many pages were considered.
    pub fn scanned(&self) -> usize {
        self.forget.len() + self.keep.len() + self.exempt.len()
    }

    /// Exemptions and how many pages each spared, in a fixed order.
    pub fn exemptions(&self) -> Vec<(Exemption, usize)> {
        let mut counts: Vec<(Exemption, usize)> = Vec::new();
        for judged in &self.exempt {
            let Verdict::Exempt { exemption, .. } = judged.verdict else {
                continue;
            };
            match counts.iter_mut().find(|(kind, _)| *kind == exemption) {
                Some((_, count)) => *count += 1,
                None => counts.push((exemption, 1)),
            }
        }
        counts.sort_by_key(|(kind, _)| *kind);
        counts
    }

    /// Pages that are exempt *and* past an expiry their author set.
    ///
    /// Two instructions from the same person that contradict each other. The
    /// sweep obeys the exemption, because deleting something marked pinned on
    /// the strength of a date is the worse mistake — and says so, because
    /// silently obeying one of two instructions is how a wiki fills up with
    /// pages someone believes are gone.
    pub fn conflicts(&self) -> impl Iterator<Item = &Judged> {
        self.exempt
            .iter()
            .filter(|judged| matches!(judged.verdict, Verdict::Exempt { expired: true, .. }))
    }

    /// The pages this plan would delete.
    pub fn doomed(&self) -> Vec<PagePath> {
        self.forget.iter().map(|j| j.row.path.clone()).collect()
    }

    /// The commit message recording what this sweep dropped.
    ///
    /// The subject line says how many and under what threshold; the body
    /// names every page and why it went, because the commit is the only place
    /// that record survives once the pages themselves are gone.
    pub fn commit_message(&self, policy: SweepPolicy, now: Timestamp) -> String {
        let count = self.forget.len();
        let plural = if count == 1 { "page" } else { "pages" };
        let mut message = format!(
            "sweep: forget {count} {plural} below retention {:.3}\n",
            policy.threshold
        );
        for judged in &self.forget {
            message.push_str(&format!(
                "\n- {} ({})",
                judged.row.path.as_str(),
                judged.reason(now)
            ));
        }
        message.push('\n');
        message
    }
}

/// Judge every page in a project, without changing anything.
pub fn plan(
    store: &Store,
    project_id: ProjectId,
    policy: SweepPolicy,
    now: Timestamp,
) -> anyhow::Result<Plan> {
    let mut plan = Plan::default();
    for row in store.sweep_rows(project_id)? {
        let verdict = judge(&row.facts, policy, now);
        let judged = Judged { row, verdict };
        match verdict {
            Verdict::Exempt { .. } => plan.exempt.push(judged),
            Verdict::Expired | Verdict::Forget { .. } => plan.forget.push(judged),
            Verdict::Keep { .. } => plan.keep.push(judged),
        }
    }

    // Weakest first, so the report opens with the pages whose loss is least
    // arguable. An expiry is not a score and does not compete with one: those
    // pages lead, in the path order the index returned them in.
    plan.forget
        .sort_by(|a, b| order_by_score(a, b, f64::NEG_INFINITY));
    plan.keep
        .sort_by(|a, b| order_by_score(a, b, f64::INFINITY));
    Ok(plan)
}

/// What one sweep actually did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Swept {
    /// Index rows removed.
    pub rows: usize,
    /// Pages named to the wiki.
    pub pages: usize,
    /// The commit recording the removal, when git had anything to record.
    pub commit: Option<String>,
}

/// Carry out a plan: drop the index rows, then remove the pages.
///
/// The order is the recoverable one (see the module docs). If the wiki step
/// fails after the rows are gone, the error says so — the memory is intact,
/// and `anamnesis reindex` puts the index back.
pub fn apply(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    plan: &Plan,
    policy: SweepPolicy,
    now: Timestamp,
) -> anyhow::Result<Swept> {
    let doomed = plan.doomed();
    if doomed.is_empty() {
        return Ok(Swept::default());
    }

    let mut rows = 0;
    for judged in &plan.forget {
        if store.delete_page(judged.row.page_id)? {
            rows += 1;
        }
    }

    let message = plan.commit_message(policy, now);
    let commit = wiki
        .delete_pages(&scope.scope, &doomed, &message)
        .map_err(|error| {
            anyhow::anyhow!(
                "{error}\n\
                 {rows} index row(s) were already dropped, but the wiki still holds every page. \
                 Nothing is lost: run `anamnesis reindex` to put the index back."
            )
        })?;

    Ok(Swept {
        rows,
        pages: doomed.len(),
        commit,
    })
}

/// Order two judged pages by score, putting the unscored ones at `unscored`.
fn order_by_score(a: &Judged, b: &Judged, unscored: f64) -> std::cmp::Ordering {
    let key = |judged: &Judged| judged.verdict.score().unwrap_or(unscored);
    key(a)
        .partial_cmp(&key(b))
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.row.path.as_str().cmp(b.row.path.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::page::{Frontmatter, Page, PagePath, PageStatus, Tier};
    use anamnesis_core::scope::resolve_scope;

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        store: Store,
        wiki: Wiki,
        scope: ResolvedScope,
    }

    fn harness() -> Harness {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");

        let data = tempfile::tempdir().expect("data");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        store.upsert_project(&scope, now()).expect("project");

        Harness {
            wiki: Wiki::open(data.path().join("wiki")).expect("wiki"),
            store,
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    fn days_ago(days: i64) -> Timestamp {
        now() - jiff::Span::new().hours(days * 24)
    }

    /// Write a page to both the wiki and the index, as it stood `age` days ago.
    fn page(harness: &Harness, path: &str, age: i64, edit: impl FnOnce(&mut Frontmatter)) -> Page {
        let mut frontmatter = Frontmatter::new("A page", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Episodic;
        edit(&mut frontmatter);
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            frontmatter,
            "Body about sqlite.",
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
        harness
            .store
            .upsert_page(&page, days_ago(age))
            .expect("upsert");
        page
    }

    fn plan_for(harness: &Harness) -> Plan {
        plan(
            &harness.store,
            harness.scope.project_id,
            SweepPolicy::default(),
            now(),
        )
        .expect("plan")
    }

    fn apply_for(harness: &Harness, plan: &Plan) -> Swept {
        apply(
            &harness.store,
            &harness.wiki,
            &harness.scope,
            plan,
            SweepPolicy::default(),
            now(),
        )
        .expect("apply")
    }

    #[test]
    fn a_plan_sorts_every_page_into_one_of_three_outcomes() {
        let harness = harness();
        page(&harness, "sessions/fresh.md", 1, |_| {});
        page(&harness, "sessions/stale.md", 400, |_| {});
        page(&harness, "notes/pinned.md", 400, |fm| fm.pinned = true);

        let plan = plan_for(&harness);
        assert_eq!(plan.scanned(), 3);
        assert_eq!(plan.forget.len(), 1);
        assert_eq!(plan.keep.len(), 1);
        assert_eq!(plan.exempt.len(), 1);
        assert_eq!(plan.forget[0].row.path.as_str(), "sessions/stale.md");
    }

    #[test]
    fn planning_changes_nothing() {
        let harness = harness();
        let doomed = page(&harness, "sessions/stale.md", 400, |_| {});
        let commits = harness.wiki.commit_count().expect("count");

        let plan = plan_for(&harness);
        assert_eq!(plan.forget.len(), 1);

        assert!(harness.wiki.exists(&harness.scope.scope, &doomed.path));
        assert_eq!(harness.wiki.commit_count().expect("count"), commits);
        assert_eq!(
            harness
                .store
                .sweep_rows(harness.scope.project_id)
                .expect("rows")
                .len(),
            1
        );
    }

    #[test]
    fn applying_removes_the_page_from_both_the_wiki_and_the_index() {
        let harness = harness();
        let doomed = page(&harness, "sessions/stale.md", 400, |_| {});
        let kept = page(&harness, "sessions/fresh.md", 1, |_| {});

        let plan = plan_for(&harness);
        let swept = apply_for(&harness, &plan);

        assert_eq!(swept.rows, 1);
        assert_eq!(swept.pages, 1);
        assert!(swept.commit.is_some());
        assert!(!harness.wiki.exists(&harness.scope.scope, &doomed.path));
        assert!(harness.wiki.exists(&harness.scope.scope, &kept.path));

        let left = harness
            .store
            .sweep_rows(harness.scope.project_id)
            .expect("rows");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path, kept.path);
    }

    #[test]
    fn a_swept_page_stops_being_retrievable() {
        let harness = harness();
        page(&harness, "sessions/stale.md", 400, |_| {});

        let plan = plan_for(&harness);
        apply_for(&harness, &plan);

        assert!(
            harness
                .store
                .query_pages(harness.scope.project_id, "sqlite", 10, now(), None)
                .expect("query")
                .is_empty()
        );
    }

    #[test]
    fn reading_a_page_saves_it_from_the_sweep() {
        // Not incidental: retrieval records the access, and the access term
        // is what "this proved useful" means. A page someone searched for
        // yesterday is not stale, whatever its age says.
        let harness = harness();
        page(&harness, "sessions/stale.md", 400, |_| {});
        assert!(
            plan_for(&harness).forget.len() == 1,
            "stale before it is read"
        );

        let hits = harness
            .store
            .query_pages(harness.scope.project_id, "sqlite", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1);

        assert!(
            plan_for(&harness).forget.is_empty(),
            "being retrieved is what keeps a page"
        );
    }

    #[test]
    fn a_second_sweep_finds_nothing_left_to_do() {
        let harness = harness();
        page(&harness, "sessions/stale.md", 400, |_| {});

        let first = plan_for(&harness);
        apply_for(&harness, &first);

        let second = plan_for(&harness);
        assert!(second.forget.is_empty());
        assert_eq!(apply_for(&harness, &second), Swept::default());
    }

    #[test]
    fn one_commit_records_every_page_a_sweep_dropped() {
        let harness = harness();
        for name in ["a", "b", "c"] {
            page(&harness, &format!("sessions/{name}.md"), 400, |_| {});
        }
        let before = harness.wiki.commit_count().expect("count");

        let plan = plan_for(&harness);
        assert_eq!(plan.forget.len(), 3);
        apply_for(&harness, &plan);

        assert_eq!(harness.wiki.commit_count().expect("count"), before + 1);
    }

    #[test]
    fn the_commit_message_names_what_went_and_why() {
        let harness = harness();
        page(&harness, "sessions/stale.md", 400, |_| {});
        page(&harness, "notes/dated.md", 1, |fm| {
            fm.expires_at = Some(days_ago(2));
        });

        let plan = plan_for(&harness);
        let message = plan.commit_message(SweepPolicy::default(), now());

        assert!(message.starts_with("sweep: forget 2 pages below retention 0.050"));
        assert!(message.contains("- sessions/stale.md (score 0."));
        assert!(message.contains("- notes/dated.md (expired 2026-08-23)"));
    }

    #[test]
    fn an_expired_page_goes_even_though_it_was_written_today() {
        let harness = harness();
        let doomed = page(&harness, "notes/dated.md", 0, |fm| {
            fm.expires_at = Some(days_ago(1));
        });

        let plan = plan_for(&harness);
        assert_eq!(plan.forget.len(), 1);
        assert_eq!(plan.forget[0].verdict, Verdict::Expired);

        apply_for(&harness, &plan);
        assert!(!harness.wiki.exists(&harness.scope.scope, &doomed.path));
    }

    #[test]
    fn an_expiry_on_a_pinned_page_is_reported_rather_than_obeyed() {
        let harness = harness();
        let kept = page(&harness, "notes/pinned.md", 0, |fm| {
            fm.pinned = true;
            fm.expires_at = Some(days_ago(1));
        });

        let plan = plan_for(&harness);
        assert!(plan.forget.is_empty());
        let conflicts: Vec<&Judged> = plan.conflicts().collect();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].row.path, kept.path);

        apply_for(&harness, &plan);
        assert!(harness.wiki.exists(&harness.scope.scope, &kept.path));
    }

    #[test]
    fn exemptions_are_counted_by_kind() {
        let harness = harness();
        page(&harness, "notes/pinned.md", 400, |fm| fm.pinned = true);
        page(&harness, "notes/how-we-deploy.md", 400, |fm| {
            fm.tier = Tier::Procedural;
        });
        page(&harness, "notes/what-it-is.md", 400, |fm| {
            fm.tier = Tier::Semantic;
        });
        page(&harness, "notes/wrong.md", 400, |fm| {
            fm.status = PageStatus::DoNotAnswerFrom;
        });

        let plan = plan_for(&harness);
        assert!(plan.forget.is_empty());
        assert_eq!(
            plan.exemptions(),
            vec![
                (Exemption::Pinned, 1),
                (Exemption::Durable, 2),
                (Exemption::KnownWrong, 1),
            ]
        );
    }

    #[test]
    fn the_weakest_page_is_reported_first() {
        let harness = harness();
        page(&harness, "sessions/older.md", 900, |_| {});
        page(&harness, "sessions/old.md", 400, |_| {});
        page(&harness, "notes/dated.md", 0, |fm| {
            fm.expires_at = Some(days_ago(1));
        });

        let plan = plan_for(&harness);
        let paths: Vec<&str> = plan
            .forget
            .iter()
            .map(|judged| judged.row.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["notes/dated.md", "sessions/older.md", "sessions/old.md"],
            "expiries lead, then the lowest scores"
        );
    }

    #[test]
    fn a_page_the_wiki_lost_is_still_swept_from_the_index() {
        let harness = harness();
        let doomed = page(&harness, "sessions/stale.md", 400, |_| {});
        std::fs::remove_file(harness.wiki.locate(&harness.scope.scope, &doomed.path))
            .expect("remove");

        let plan = plan_for(&harness);
        let swept = apply_for(&harness, &plan);

        assert_eq!(swept.rows, 1);
        assert!(
            harness
                .store
                .sweep_rows(harness.scope.project_id)
                .expect("rows")
                .is_empty()
        );
    }

    #[test]
    fn a_higher_threshold_forgets_more() {
        let harness = harness();
        page(&harness, "sessions/middling.md", 60, |_| {});

        let lenient = plan(
            &harness.store,
            harness.scope.project_id,
            SweepPolicy::default(),
            now(),
        )
        .expect("plan");
        assert!(lenient.forget.is_empty());

        let strict = plan(
            &harness.store,
            harness.scope.project_id,
            SweepPolicy {
                threshold: 0.5,
                ..SweepPolicy::default()
            },
            now(),
        )
        .expect("plan");
        assert_eq!(strict.forget.len(), 1);
    }

    #[test]
    fn an_empty_project_sweeps_to_nothing() {
        let harness = harness();
        let plan = plan_for(&harness);
        assert_eq!(plan.scanned(), 0);
        assert_eq!(apply_for(&harness, &plan), Swept::default());
        assert_eq!(harness.wiki.commit_count().expect("count"), 0);
    }
}
