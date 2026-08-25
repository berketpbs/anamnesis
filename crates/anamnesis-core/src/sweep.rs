//! Deciding which pages a sweep forgets, and which it must not touch.
//!
//! [`crate::decay`] answers one question — how strongly does this page resist
//! being forgotten — in pure arithmetic that knows nothing about pages. This
//! module is where that number meets the rules that outrank it:
//!
//! * **Pinned** pages are never swept. The schema calls this a first-order
//!   retention control, and it is: someone wrote `pinned: true` to mean "this
//!   survives", not "score this generously".
//! * **Durable tiers** — `semantic` and `procedural` — persist until
//!   superseded. What a project *is* does not become false because nobody
//!   searched for it this quarter.
//! * **Canonical** pages are the declared answer on their subject. Deleting
//!   the one page a reader is meant to trust, because it went unread, would
//!   leave the subject covered only by the scattered pages that outlived it.
//! * **`do-not-answer-from`** pages are exempt for a less obvious reason: a
//!   page recording a known-wrong belief is almost never *retrieved*, so
//!   scoring alone would sweep it first — and it would go precisely when it
//!   has been quiet long enough for someone to make the mistake again. Its
//!   value is in being rare and decisive, which is the opposite of what the
//!   access term rewards.
//!
//! An explicit `expires_at` in the past forgets a page whatever its score:
//! that is a deadline its author wrote down, not an estimate. It does not
//! override an exemption, though — a pinned page that also carries a passed
//! expiry is a contradiction between two things the same author wrote, and
//! the sweep reports it rather than picking a winner in silence.

use jiff::Timestamp;

use crate::decay::{DecayInputs, DecayParams, retention_score};
use crate::page::{PageStatus, Tier};

/// Milliseconds in a day, for turning two timestamps into a decay input.
const MS_PER_DAY: f64 = 86_400_000.0;

/// Everything a sweep needs to know about one page.
///
/// Deliberately not a `Page`: the body is irrelevant here, and the two facts
/// that matter most — how often the page has been read, and when — live in
/// the index rather than in the markdown.
#[derive(Debug, Clone, PartialEq)]
pub struct PageFacts {
    /// Exempt from the sweep by explicit instruction.
    pub pinned: bool,
    /// Declared the authoritative page on its subject.
    pub canonical: bool,
    /// Temporal tier; durable tiers do not decay.
    pub tier: Tier,
    /// Trust level.
    pub status: PageStatus,
    /// Importance assigned when the page was written.
    pub salience: f64,
    /// When the page was last written. Editing a page renews it: the content
    /// a reader would find is that recent, whatever the first version said.
    pub written_at: Timestamp,
    /// How many times retrieval has returned this page.
    pub access_count: u32,
    /// When retrieval last returned it, if ever.
    pub last_accessed_at: Option<Timestamp>,
    /// Deadline written by the page's author.
    pub expires_at: Option<Timestamp>,
}

/// Why a page is out of the sweep's reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Exemption {
    /// `pinned: true` in the page's frontmatter.
    Pinned,
    /// A `semantic` or `procedural` page, which persists until superseded.
    Durable,
    /// `canonical: true`: the declared answer on its subject.
    Canonical,
    /// `status: do-not-answer-from`: a known-wrong claim kept visible.
    KnownWrong,
}

impl Exemption {
    /// One word naming the exemption, for a report someone reads.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Durable => "durable",
            Self::Canonical => "canonical",
            Self::KnownWrong => "known-wrong",
        }
    }
}

/// What a sweep decided about one page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Out of reach, whatever the score.
    Exempt {
        /// Which rule protects it.
        exemption: Exemption,
        /// Whether it also carries an expiry that has already passed — a
        /// contradiction worth reporting, never resolved silently.
        expired: bool,
    },
    /// Past the deadline its author wrote. Forgotten regardless of score.
    Expired,
    /// Scored below the threshold.
    Forget {
        /// The retention score that condemned it.
        score: f64,
    },
    /// Scored at or above the threshold.
    Keep {
        /// The retention score that saved it.
        score: f64,
    },
}

impl Verdict {
    /// Whether this verdict means the page goes.
    pub fn forgets(&self) -> bool {
        matches!(self, Self::Expired | Self::Forget { .. })
    }

    /// The retention score, where one was computed.
    ///
    /// `None` for pages that never reached the arithmetic: scoring an exempt
    /// page invites a report that ranks it alongside pages the number
    /// actually governs.
    pub fn score(&self) -> Option<f64> {
        match self {
            Self::Forget { score } | Self::Keep { score } => Some(*score),
            Self::Exempt { .. } | Self::Expired => None,
        }
    }
}

/// The coefficients and cutoff one sweep runs with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepPolicy {
    /// Decay coefficients.
    pub params: DecayParams,
    /// Score below which an unexempt page is forgotten.
    pub threshold: f64,
}

impl SweepPolicy {
    /// Default cutoff: roughly four months for a page nobody ever reads.
    ///
    /// Under the default half-life an unread page of salience `1.0` crosses
    /// `0.05` at `ln(20) / λ` ≈ 130 days. Long enough that a quiet quarter
    /// does not erase a project's memory of itself; short enough that the
    /// corpus does not grow without bound.
    pub const DEFAULT_THRESHOLD: f64 = 0.05;
}

impl Default for SweepPolicy {
    fn default() -> Self {
        Self {
            params: DecayParams::default(),
            threshold: Self::DEFAULT_THRESHOLD,
        }
    }
}

impl PageFacts {
    /// The decay inputs this page presents at `now`.
    ///
    /// A page never retrieved reports its full age as time-since-access,
    /// which is what [`DecayInputs::unread`] means: the access term is zero
    /// either way, and the alternative — treating "never" as "just now" —
    /// would make an unread page indistinguishable from a popular one.
    pub fn inputs(&self, now: Timestamp) -> DecayInputs {
        let age_days = days_between(self.written_at, now);
        match self.last_accessed_at {
            Some(read) if self.access_count > 0 => DecayInputs {
                salience: self.salience,
                age_days,
                access_count: self.access_count,
                days_since_access: days_between(read, now),
            },
            _ => DecayInputs::unread(self.salience, age_days),
        }
    }

    /// Which rule, if any, puts this page out of the sweep's reach.
    pub fn exemption(&self) -> Option<Exemption> {
        if self.pinned {
            Some(Exemption::Pinned)
        } else if self.tier.is_durable() {
            Some(Exemption::Durable)
        } else if self.canonical {
            Some(Exemption::Canonical)
        } else if matches!(self.status, PageStatus::DoNotAnswerFrom) {
            Some(Exemption::KnownWrong)
        } else {
            None
        }
    }

    /// Whether the author's own deadline has passed.
    pub fn has_expired(&self, now: Timestamp) -> bool {
        self.expires_at.is_some_and(|deadline| now >= deadline)
    }
}

/// Judge one page against a policy.
pub fn judge(facts: &PageFacts, policy: SweepPolicy, now: Timestamp) -> Verdict {
    if let Some(exemption) = facts.exemption() {
        return Verdict::Exempt {
            exemption,
            expired: facts.has_expired(now),
        };
    }
    if facts.has_expired(now) {
        return Verdict::Expired;
    }

    let score = retention_score(facts.inputs(now), policy.params);
    if score < policy.threshold {
        Verdict::Forget { score }
    } else {
        Verdict::Keep { score }
    }
}

/// Days between two instants, as the decay formula counts them.
///
/// Negative when `later` precedes `earlier`; [`retention_score`] clamps that
/// rather than treating clock skew as evidence of importance.
fn days_between(earlier: Timestamp, later: Timestamp) -> f64 {
    (later.as_millisecond() - earlier.as_millisecond()) as f64 / MS_PER_DAY
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-08-25T12:00:00Z".parse().expect("timestamp")
    }

    fn days_ago(days: i64) -> Timestamp {
        now() - jiff::Span::new().hours(days * 24)
    }

    fn days_ahead(days: i64) -> Timestamp {
        now() + jiff::Span::new().hours(days * 24)
    }

    /// An ordinary episodic page written `age` days ago and never read.
    fn unread(age: i64) -> PageFacts {
        PageFacts {
            pinned: false,
            canonical: false,
            tier: Tier::Episodic,
            status: PageStatus::Active,
            salience: 1.0,
            written_at: days_ago(age),
            access_count: 0,
            last_accessed_at: None,
            expires_at: None,
        }
    }

    fn verdict(facts: &PageFacts) -> Verdict {
        judge(facts, SweepPolicy::default(), now())
    }

    #[test]
    fn a_page_written_today_is_kept() {
        assert!(matches!(verdict(&unread(0)), Verdict::Keep { .. }));
    }

    #[test]
    fn an_unread_page_falls_below_the_default_threshold_after_four_months() {
        assert!(matches!(verdict(&unread(120)), Verdict::Keep { .. }));
        assert!(matches!(verdict(&unread(140)), Verdict::Forget { .. }));
    }

    #[test]
    fn being_read_keeps_a_page_the_score_would_otherwise_forget() {
        let mut facts = unread(200);
        assert!(verdict(&facts).forgets());

        facts.access_count = 5;
        facts.last_accessed_at = Some(days_ago(3));
        assert!(!verdict(&facts).forgets());
    }

    #[test]
    fn a_read_recorded_without_a_count_does_not_manufacture_an_access_term() {
        // A row with `last_accessed_at` set but `access_count` zero is not
        // something this system writes; if one appears, the honest reading is
        // "never read", not "read ln(1 + 0) times".
        let mut facts = unread(200);
        facts.last_accessed_at = Some(days_ago(1));
        assert_eq!(facts.inputs(now()), DecayInputs::unread(1.0, 200.0));
    }

    #[test]
    fn pinned_pages_never_reach_the_score() {
        let mut facts = unread(3_650);
        facts.pinned = true;
        assert_eq!(
            verdict(&facts),
            Verdict::Exempt {
                exemption: Exemption::Pinned,
                expired: false
            }
        );
    }

    #[test]
    fn durable_tiers_persist_until_superseded() {
        for tier in [Tier::Semantic, Tier::Procedural] {
            let mut facts = unread(3_650);
            facts.tier = tier;
            assert!(matches!(
                verdict(&facts),
                Verdict::Exempt {
                    exemption: Exemption::Durable,
                    ..
                }
            ));
        }
    }

    #[test]
    fn working_and_episodic_pages_do_decay() {
        for tier in [Tier::Working, Tier::Episodic] {
            let mut facts = unread(3_650);
            facts.tier = tier;
            assert!(verdict(&facts).forgets(), "{tier:?} survived");
        }
    }

    #[test]
    fn the_canonical_page_on_a_subject_is_not_swept_for_being_quiet() {
        let mut facts = unread(3_650);
        facts.canonical = true;
        assert!(matches!(
            verdict(&facts),
            Verdict::Exempt {
                exemption: Exemption::Canonical,
                ..
            }
        ));
    }

    #[test]
    fn a_known_wrong_page_is_exempt_although_nobody_reads_it() {
        let mut facts = unread(3_650);
        facts.status = PageStatus::DoNotAnswerFrom;
        assert!(matches!(
            verdict(&facts),
            Verdict::Exempt {
                exemption: Exemption::KnownWrong,
                ..
            }
        ));
    }

    #[test]
    fn superseded_pages_are_ordinary_candidates() {
        let mut facts = unread(3_650);
        facts.status = PageStatus::Superseded;
        assert!(verdict(&facts).forgets());
    }

    #[test]
    fn an_expiry_in_the_past_forgets_a_page_the_score_would_keep() {
        let mut facts = unread(0);
        assert!(!verdict(&facts).forgets());

        facts.expires_at = Some(days_ago(1));
        assert_eq!(verdict(&facts), Verdict::Expired);
    }

    #[test]
    fn an_expiry_in_the_future_changes_nothing() {
        let mut facts = unread(0);
        facts.expires_at = Some(days_ahead(30));
        assert!(matches!(verdict(&facts), Verdict::Keep { .. }));
    }

    #[test]
    fn an_expiry_on_an_exempt_page_is_reported_rather_than_resolved() {
        let mut facts = unread(0);
        facts.pinned = true;
        facts.expires_at = Some(days_ago(1));
        assert_eq!(
            verdict(&facts),
            Verdict::Exempt {
                exemption: Exemption::Pinned,
                expired: true
            }
        );
    }

    #[test]
    fn salience_buys_a_page_time() {
        let mut facts = unread(140);
        assert!(verdict(&facts).forgets());

        facts.salience = 3.0;
        assert!(!verdict(&facts).forgets());
    }

    #[test]
    fn a_page_written_in_the_future_is_kept_rather_than_scored_negatively() {
        // Clock skew between a hook host and the server is ordinary; it must
        // not be able to change what gets deleted.
        let mut facts = unread(0);
        facts.written_at = days_ahead(30);
        assert!(matches!(verdict(&facts), Verdict::Keep { .. }));
    }

    #[test]
    fn only_scored_verdicts_carry_a_score() {
        assert!(verdict(&unread(0)).score().is_some());

        let mut pinned = unread(0);
        pinned.pinned = true;
        assert!(verdict(&pinned).score().is_none());
    }

    #[test]
    fn a_zero_threshold_forgets_nothing() {
        let policy = SweepPolicy {
            threshold: 0.0,
            ..SweepPolicy::default()
        };
        assert!(!judge(&unread(36_500), policy, now()).forgets());
    }
}
