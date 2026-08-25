//! What auto-improve notices about a memory, and what it proposes doing.
//!
//! A memory that only accumulates gets worse. [`crate::sweep`] handles one
//! half of that — pages nobody needs are forgotten. This module is the other
//! half: pages that have *earned* something, and gaps the wiki keeps pointing
//! at, are turned into proposals a person (or, when they have said so, the
//! system itself) can act on.
//!
//! Two rules, both computed from signals the system already records rather
//! than from a model's opinion:
//!
//! * **Promote a page that keeps being read.** Retrieval records every hit, so
//!   an `episodic` page that several later sessions went back to is not a
//!   session note any more — it is knowledge that happens to be filed as one.
//!   Promoting it to `semantic` says so, and has a second effect worth being
//!   deliberate about: durable tiers are exempt from the sweep, so this is how
//!   a page stops being forgettable by proving itself rather than by someone
//!   remembering to pin it.
//! * **Notice a page the wiki keeps asking for.** A `[[link]]` to a page that
//!   does not exist is a marker of intent, not an error. One of them is
//!   someone's note to self. Several, from different pages, is the wiki
//!   telling you which page is missing.
//!
//! Proposals are *observations of a condition*, which is why they carry no
//! deadline and no priority. The condition either still holds at the next
//! pass, or it does not and the proposal is resolved — including when a
//! person fixed it themselves, which is the outcome to hope for.

use jiff::Timestamp;

use crate::ids::PageId;
use crate::page::{PagePath, PageStatus, Tier};

/// Reads before an episodic page is considered proven.
///
/// Three, not one: retrieval returns a page for being *relevant*, and the
/// first hit is often the search that found it once. A page three different
/// searches came back to is a different claim.
pub const PROMOTE_AFTER_READS: u32 = 3;

/// Days a page must have existed before it can be promoted.
///
/// A page read three times on the day it was written is one session working,
/// not knowledge outliving its session.
pub const PROMOTE_MIN_AGE_DAYS: f64 = 14.0;

/// Days since the last read, beyond which "proven useful" has gone stale.
pub const PROMOTE_RECENT_READ_DAYS: f64 = 60.0;

/// Distinct pages that must link to a missing target before it is proposed.
pub const MISSING_PAGE_MIN_SOURCES: usize = 2;

/// Milliseconds in a day.
const MS_PER_DAY: f64 = 86_400_000.0;

/// What a proposal asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProposalKind {
    /// Move a proven episodic page into the `semantic` tier.
    PromoteTier,
    /// Write a page several others already link to.
    WriteMissingPage,
}

impl ProposalKind {
    /// Canonical identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromoteTier => "promote-tier",
            Self::WriteMissingPage => "write-missing-page",
        }
    }

    /// Recover a kind from its stored form.
    ///
    /// Returns `None` rather than falling back to a default, unlike the other
    /// enums in this crate. A default here would mean carrying out one action
    /// because a row asked for another, and the whole point of a proposal is
    /// that it says exactly what it wants done.
    pub fn from_storage(raw: &str) -> Option<Self> {
        match raw {
            "promote-tier" => Some(Self::PromoteTier),
            "write-missing-page" => Some(Self::WriteMissingPage),
            _ => None,
        }
    }

    /// Whether the system can carry this out on its own.
    ///
    /// Promotion is a change to one frontmatter field, so it can. Writing a
    /// missing page cannot be mechanical: nobody — and no schedule — can
    /// invent what the page should say.
    pub fn is_applicable(&self) -> bool {
        matches!(self, Self::PromoteTier)
    }

    /// One line naming what applying this would do.
    pub fn action(&self) -> &'static str {
        match self {
            Self::PromoteTier => "promote to the semantic tier",
            Self::WriteMissingPage => "write the page",
        }
    }
}

/// Where a proposal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProposalState {
    /// The condition holds and nothing has been decided.
    #[default]
    Open,
    /// The system carried it out.
    Applied,
    /// A person said no. Never proposed again.
    Dismissed,
    /// The condition stopped holding without the system acting — usually
    /// because someone did the thing themselves.
    Resolved,
}

impl ProposalState {
    /// Canonical identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Applied => "applied",
            Self::Dismissed => "dismissed",
            Self::Resolved => "resolved",
        }
    }

    /// Recover a state from its stored form, the inverse of [`Self::as_str`].
    ///
    /// Falls back to [`Self::Open`]: the schema's `CHECK` means an
    /// unrecognised value cannot come from a database this code wrote, and
    /// showing a proposal that was already decided is a smaller harm than
    /// hiding one that was not.
    pub fn from_storage(raw: &str) -> Self {
        match raw {
            "applied" => Self::Applied,
            "dismissed" => Self::Dismissed,
            "resolved" => Self::Resolved,
            _ => Self::Open,
        }
    }

    /// Whether this proposal is still waiting on something.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }
}

/// What a pass wants done, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// What it asks for.
    pub kind: ProposalKind,
    /// What it is about: a page path, or the target nothing answers to.
    ///
    /// Together with the kind this identifies the proposal, which is what
    /// keeps a pass from filing the same finding twice.
    pub subject: String,
    /// The page this concerns, when the subject is one that exists.
    pub page_id: Option<PageId>,
    /// The evidence, in the words a report uses.
    pub rationale: String,
}

/// One page, as auto-improve reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageStats {
    /// Identifies the page.
    pub page_id: PageId,
    /// Project-relative path.
    pub path: PagePath,
    /// Temporal tier.
    pub tier: Tier,
    /// Trust status.
    pub status: PageStatus,
    /// Whether this is the head of its supersession chain.
    pub is_latest: bool,
    /// When the page was last written.
    pub written_at: Timestamp,
    /// How many times retrieval has returned it.
    pub access_count: u32,
    /// When retrieval last returned it.
    pub last_accessed_at: Option<Timestamp>,
}

/// A link target that no page answers to, and the pages asking for it.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingTarget {
    /// The target exactly as it was written between the brackets.
    pub target: String,
    /// Pages linking to it, in path order.
    pub sources: Vec<PagePath>,
}

/// Everything one pass reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// Every page in the project.
    pub pages: Vec<PageStats>,
    /// Every link target with no page behind it.
    pub missing: Vec<MissingTarget>,
}

/// Propose what a project's memory should do about itself.
///
/// Deterministic and ordered: the same facts produce the same proposals in
/// the same order, so two passes over an unchanged memory agree, and a
/// proposal's identity never depends on the order rows came back in.
pub fn propose(facts: &Facts, now: Timestamp) -> Vec<Proposal> {
    let mut proposals: Vec<Proposal> = facts
        .pages
        .iter()
        .filter_map(|page| propose_promotion(page, now))
        .chain(facts.missing.iter().filter_map(propose_missing_page))
        .collect();

    proposals.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.subject.cmp(&b.subject)));
    proposals
}

/// Propose promoting one page, if it has earned it.
pub fn propose_promotion(page: &PageStats, now: Timestamp) -> Option<Proposal> {
    if page.tier != Tier::Episodic || page.status != PageStatus::Active || !page.is_latest {
        return None;
    }
    if page.access_count < PROMOTE_AFTER_READS {
        return None;
    }

    let age_days = days_between(page.written_at, now);
    if age_days < PROMOTE_MIN_AGE_DAYS {
        return None;
    }

    let last_read = page.last_accessed_at?;
    let since_read = days_between(last_read, now);
    if since_read > PROMOTE_RECENT_READ_DAYS {
        return None;
    }

    Some(Proposal {
        kind: ProposalKind::PromoteTier,
        subject: page.path.as_str().to_owned(),
        page_id: Some(page.page_id),
        rationale: format!(
            "read {} times, most recently {} days ago, and still episodic {} days after it was written",
            page.access_count,
            since_read.max(0.0).round() as i64,
            age_days.round() as i64
        ),
    })
}

/// Propose writing a page the wiki keeps pointing at.
pub fn propose_missing_page(missing: &MissingTarget) -> Option<Proposal> {
    if missing.sources.len() < MISSING_PAGE_MIN_SOURCES {
        return None;
    }

    Some(Proposal {
        kind: ProposalKind::WriteMissingPage,
        subject: missing.target.clone(),
        page_id: None,
        rationale: format!(
            "{} pages link to it and no page answers to it: {}",
            missing.sources.len(),
            missing
                .sources
                .iter()
                .map(PagePath::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// Days between two instants.
fn days_between(earlier: Timestamp, later: Timestamp) -> f64 {
    (later.as_millisecond() - earlier.as_millisecond()) as f64 / MS_PER_DAY
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProjectId;

    fn now() -> Timestamp {
        "2026-08-25T12:00:00Z".parse().expect("timestamp")
    }

    fn days_ago(days: i64) -> Timestamp {
        now() - jiff::Span::new().hours(days * 24)
    }

    fn path(raw: &str) -> PagePath {
        PagePath::parse(raw).expect("path")
    }

    /// A page read `reads` times, last read `since` days ago.
    fn read_page(reads: u32, age: i64, since: i64) -> PageStats {
        PageStats {
            page_id: PageId::derive(ProjectId::from_uuid(uuid::Uuid::nil()), &path("s/a.md")),
            path: path("s/a.md"),
            tier: Tier::Episodic,
            status: PageStatus::Active,
            is_latest: true,
            written_at: days_ago(age),
            access_count: reads,
            last_accessed_at: Some(days_ago(since)),
        }
    }

    #[test]
    fn a_page_several_sessions_came_back_to_is_proposed_for_promotion() {
        let proposal = propose_promotion(&read_page(4, 40, 2), now()).expect("proposed");
        assert_eq!(proposal.kind, ProposalKind::PromoteTier);
        assert_eq!(proposal.subject, "s/a.md");
        assert!(proposal.rationale.contains("read 4 times"));
        assert!(proposal.rationale.contains("2 days ago"));
        assert!(proposal.rationale.contains("40 days"));
    }

    #[test]
    fn one_or_two_reads_are_not_evidence() {
        for reads in 0..PROMOTE_AFTER_READS {
            assert!(propose_promotion(&read_page(reads, 40, 2), now()).is_none());
        }
        assert!(propose_promotion(&read_page(PROMOTE_AFTER_READS, 40, 2), now()).is_some());
    }

    #[test]
    fn a_page_read_three_times_on_the_day_it_was_written_is_one_session_working() {
        assert!(propose_promotion(&read_page(5, 1, 0), now()).is_none());
        assert!(propose_promotion(&read_page(5, 20, 0), now()).is_some());
    }

    #[test]
    fn proven_useful_goes_stale() {
        assert!(propose_promotion(&read_page(9, 400, 30), now()).is_some());
        assert!(propose_promotion(&read_page(9, 400, 200), now()).is_none());
    }

    #[test]
    fn only_active_episodic_heads_are_promoted() {
        let mut durable = read_page(9, 40, 2);
        durable.tier = Tier::Semantic;
        assert!(propose_promotion(&durable, now()).is_none());

        let mut scratch = read_page(9, 40, 2);
        scratch.tier = Tier::Working;
        assert!(propose_promotion(&scratch, now()).is_none());

        let mut wrong = read_page(9, 40, 2);
        wrong.status = PageStatus::DoNotAnswerFrom;
        assert!(propose_promotion(&wrong, now()).is_none());

        let mut replaced = read_page(9, 40, 2);
        replaced.is_latest = false;
        assert!(propose_promotion(&replaced, now()).is_none());
    }

    #[test]
    fn a_count_without_a_read_time_proves_nothing() {
        // Not a row this system writes; treated as never read rather than as
        // read at an unknown time, because the alternative is promoting on
        // evidence nobody can point at.
        let mut page = read_page(9, 40, 2);
        page.last_accessed_at = None;
        assert!(propose_promotion(&page, now()).is_none());
    }

    #[test]
    fn one_page_asking_for_a_missing_page_is_a_note_to_self() {
        let single = MissingTarget {
            target: "gotchas/windows-bom.md".to_owned(),
            sources: vec![path("s/a.md")],
        };
        assert!(propose_missing_page(&single).is_none());
    }

    #[test]
    fn several_pages_asking_for_the_same_missing_page_is_a_gap() {
        let missing = MissingTarget {
            target: "gotchas/windows-bom.md".to_owned(),
            sources: vec![path("s/a.md"), path("s/b.md"), path("s/c.md")],
        };
        let proposal = propose_missing_page(&missing).expect("proposed");
        assert_eq!(proposal.kind, ProposalKind::WriteMissingPage);
        assert_eq!(proposal.subject, "gotchas/windows-bom.md");
        assert_eq!(proposal.page_id, None);
        assert!(proposal.rationale.contains("3 pages"));
        assert!(proposal.rationale.contains("s/b.md"));
    }

    #[test]
    fn a_pass_orders_its_proposals_the_same_way_every_time() {
        let facts = Facts {
            pages: vec![
                PageStats {
                    path: path("s/z.md"),
                    ..read_page(9, 40, 2)
                },
                PageStats {
                    path: path("s/a.md"),
                    ..read_page(9, 40, 2)
                },
            ],
            missing: vec![MissingTarget {
                target: "gotchas/windows-bom.md".to_owned(),
                sources: vec![path("s/a.md"), path("s/b.md")],
            }],
        };

        let proposals = propose(&facts, now());
        let subjects: Vec<&str> = proposals.iter().map(|p| p.subject.as_str()).collect();
        assert_eq!(
            subjects,
            vec!["s/a.md", "s/z.md", "gotchas/windows-bom.md"],
            "promotions first, each kind in subject order"
        );
    }

    #[test]
    fn an_empty_memory_proposes_nothing() {
        assert!(propose(&Facts::default(), now()).is_empty());
    }

    #[test]
    fn only_promotion_is_something_the_system_can_do_itself() {
        assert!(ProposalKind::PromoteTier.is_applicable());
        assert!(!ProposalKind::WriteMissingPage.is_applicable());
    }

    #[test]
    fn kinds_and_states_round_trip_through_storage() {
        for kind in [ProposalKind::PromoteTier, ProposalKind::WriteMissingPage] {
            assert_eq!(ProposalKind::from_storage(kind.as_str()), Some(kind));
        }
        assert_eq!(ProposalKind::from_storage("invent-a-page"), None);

        for state in [
            ProposalState::Open,
            ProposalState::Applied,
            ProposalState::Dismissed,
            ProposalState::Resolved,
        ] {
            assert_eq!(ProposalState::from_storage(state.as_str()), state);
        }
    }
}
