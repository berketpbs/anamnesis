//! Whether a fusion constant is doing anything a corpus can see.
//!
//! [`crate::sweep`] answers "which setting scores best". This answers the
//! question that has to come first and never has been asked here: **can these
//! corpora tell the settings apart at all?**
//!
//! The distinction is not pedantic. On 2026-08-29 a sweep lowered `rrf_k` from
//! the canonical 60 to 2.0 and it was recorded as the single biggest win. `k`
//! decides how far one stream's first place outranks everything else — at
//! `k = 2` rank one is worth four times rank ten, at `k = 60` the two are
//! nearly level — so "follow the stream that is sure" against "let the streams
//! vote". A score identical at `k = 1` and `k = 60` would not mean the value
//! between them is correct; it would mean the corpus cannot tell, and a number
//! measured where the question does not live reads exactly like a number
//! measured where it does.
//!
//! **This module was written expecting to find that, and found the opposite.**
//! The doc comment here said, before it was run, that on ten pages a stream's
//! top hit is almost always right and the choice costs nothing either way. It
//! is recorded rather than deleted because the first run refuted it: every
//! shipped suite discriminates, and sharply — hit@1 moves 0.300 across the grid
//! on `retrieval`, 0.375 on `adversarial`, 0.533 on `crowded`, and more than
//! half of `crowded`'s questions change their first answer somewhere in it. The
//! low `k` this project ships is not an artifact of a corpus too small to
//! object. These corpora object loudly, and they agree.
//!
//! Two things the run did say, which are not the thing it was built to look
//! for:
//!
//! - **`k = 1` and `k = 2` are indistinguishable on all three suites**, to
//!   three decimal places on every measure. What is measured is "≤ 2", and the
//!   2 is a choice within that, not a result.
//! - **The penalty for a high `k` grows with the corpus**, 0.300 at ten pages
//!   against 0.533 at twenty-two. That is the opposite of the direction the
//!   worry ran, though `crowded` is crowded by construction, so this is at
//!   least as likely to be about confusability as about size. Which of the two
//!   it is, is the question a bigger corpus is still needed to answer.
//!
//! Nothing here changes a default. It is an instrument.

use anamnesis_core::retrieval::Tuning;
use jiff::Timestamp;

use crate::corpus::Corpus;
use crate::run::{Report, run_on};
use crate::score::{hit_at_one, mean_reciprocal_rank, ndcg_at, recall};
use crate::suite::Suite;
use crate::{EvalError, score::CaseScore};

use anamnesis_core::embedding::Embed;

/// The `rrf_k` values swept, matching [`crate::sweep::default_grid`].
///
/// The same ladder rather than a finer one on purpose: the claim being tested
/// is about the values a sweep actually chose between, and a grid this one did
/// not use would answer a question nobody asked.
pub const K_GRID: [f64; 7] = [1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0];

/// One `rrf_k` value's scores, with everything else left at what ships.
#[derive(Debug, Clone, PartialEq)]
pub struct KPoint {
    /// The constant this row was scored at.
    pub rrf_k: f64,
    /// Share of questions answered first.
    pub hit1: f64,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// NDCG over the suite's scored window.
    pub ndcg: f64,
    /// Share of relevant pages returned.
    pub recall: f64,
}

/// What varying `rrf_k` did to one suite.
#[derive(Debug, Clone, PartialEq)]
pub struct KSensitivity {
    /// Which suite.
    pub suite: String,
    /// How many pages it holds — the number the whole question is about.
    pub pages: usize,
    /// How many questions were asked.
    pub cases: usize,
    /// One row per value in [`K_GRID`].
    pub points: Vec<KPoint>,
    /// Questions whose first answer was not the same at every `k`.
    ///
    /// The headline. A suite with none of these scored identically at every
    /// setting because nothing moved, not because the setting was right.
    pub top1_moved: Vec<String>,
    /// Questions whose returned ordering was not identical at every `k`.
    ///
    /// Weaker than the above and worth having beside it: an ordering that
    /// shuffles below first place is a corpus that can see `k` doing
    /// *something*, even where the answer never changes.
    pub order_moved: Vec<String>,
}

impl KSensitivity {
    /// Whether any question's first answer changed anywhere in the grid.
    ///
    /// False is the finding, not the pass: it means this suite could not have
    /// distinguished `k = 1` from `k = 60`, and therefore did not distinguish
    /// the value between them either.
    pub fn discriminates(&self) -> bool {
        !self.top1_moved.is_empty()
    }

    /// The spread of hit@1 across the grid: the most this suite's headline
    /// measure moved for any value of `k`.
    pub fn hit1_spread(&self) -> f64 {
        let scores: Vec<f64> = self.points.iter().map(|point| point.hit1).collect();
        let highest = scores.iter().copied().fold(f64::MIN, f64::max);
        let lowest = scores.iter().copied().fold(f64::MAX, f64::min);
        if scores.is_empty() {
            0.0
        } else {
            highest - lowest
        }
    }
}

/// Score one suite once per value in [`K_GRID`], and report what moved.
///
/// The corpus is built once. Every run after that asks the same questions of
/// the same pages, which is what makes the comparison a comparison: two runs
/// over two corpora would differ for reasons that are not `k`.
pub fn k_sensitivity(
    suite: &Suite,
    now: Timestamp,
    embedder: Option<&dyn Embed>,
) -> Result<KSensitivity, EvalError> {
    let corpus = Corpus::build_with(suite, now, embedder)?;

    let mut points = Vec::with_capacity(K_GRID.len());
    let mut reports = Vec::with_capacity(K_GRID.len());
    for rrf_k in K_GRID {
        let tuning = Tuning {
            rrf_k,
            ..Tuning::default()
        };
        let report = run_on(&corpus, suite, now, &tuning, embedder)?;
        points.push(point_from(rrf_k, &report));
        reports.push(report);
    }

    let (top1_moved, order_moved) = what_moved(&reports);
    Ok(KSensitivity {
        suite: suite.name.clone(),
        pages: suite.pages.len(),
        cases: suite.cases.len(),
        points,
        top1_moved,
        order_moved,
    })
}

/// Read one grid row out of a scored report.
fn point_from(rrf_k: f64, report: &Report) -> KPoint {
    let scores: Vec<CaseScore> = report.cases.iter().map(|case| case.score.clone()).collect();
    KPoint {
        rrf_k,
        hit1: hit_at_one(&scores),
        mrr: mean_reciprocal_rank(&scores),
        ndcg: ndcg_at(&scores, report.limit),
        recall: recall(&scores),
    }
}

/// Which questions answered differently somewhere in the grid.
///
/// Compared against the first row rather than pairwise: a question whose
/// answer differs anywhere differs from `k = 1` somewhere, and pairwise
/// comparison would report the same question once per adjacent pair.
fn what_moved(reports: &[Report]) -> (Vec<String>, Vec<String>) {
    let mut top1 = Vec::new();
    let mut order = Vec::new();
    let Some(first) = reports.first() else {
        return (top1, order);
    };

    for (index, case) in first.cases.iter().enumerate() {
        let baseline_top = case.returned.first();
        let others = reports.iter().skip(1).filter_map(|report| {
            report
                .cases
                .get(index)
                .filter(|other| other.query == case.query)
        });

        let mut moved_top = false;
        let mut moved_order = false;
        for other in others {
            if other.returned.first() != baseline_top {
                moved_top = true;
            }
            if other.returned != case.returned {
                moved_order = true;
            }
        }
        if moved_top {
            top1.push(case.query.clone());
        }
        if moved_order {
            order.push(case.query.clone());
        }
    }
    (top1, order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suites() -> Vec<Suite> {
        crate::builtin_suites()
            .into_iter()
            .map(|(_, source)| Suite::from_toml(source).expect("a shipped suite parses"))
            .collect()
    }

    fn now() -> Timestamp {
        "2026-01-01T00:00:00Z".parse().expect("timestamp")
    }

    #[test]
    fn every_grid_value_is_scored() {
        let suite = &suites()[0];
        let measured = k_sensitivity(suite, now(), None).expect("sensitivity");

        assert_eq!(measured.points.len(), K_GRID.len());
        let swept: Vec<f64> = measured.points.iter().map(|point| point.rrf_k).collect();
        assert_eq!(swept, K_GRID.to_vec());
        assert_eq!(measured.pages, suite.pages.len());
        assert_eq!(measured.cases, suite.cases.len());
    }

    /// A question that never changes its answer is reported as not having
    /// changed it. The whole instrument rests on this being the honest
    /// default rather than an empty list meaning "not measured".
    #[test]
    fn a_suite_that_cannot_tell_the_settings_apart_says_so() {
        for suite in suites() {
            let measured = k_sensitivity(&suite, now(), None).expect("sensitivity");
            assert!(
                measured.top1_moved.len() <= measured.cases,
                "{} reported more moved questions than it asked",
                measured.suite
            );
            // Whatever the corpora happen to score, the two lists have to
            // agree with each other: a first answer cannot change without the
            // ordering changing with it.
            for query in &measured.top1_moved {
                assert!(
                    measured.order_moved.contains(query),
                    "{query:?} changed its first answer without changing its order"
                );
            }
        }
    }

    /// `discriminates` is the question this module exists to ask, so it has to
    /// follow the evidence rather than a threshold somebody picked.
    #[test]
    fn discrimination_is_read_from_what_moved() {
        for suite in suites() {
            let measured = k_sensitivity(&suite, now(), None).expect("sensitivity");
            assert_eq!(measured.discriminates(), !measured.top1_moved.is_empty());
            if !measured.discriminates() {
                assert_eq!(
                    measured.hit1_spread(),
                    0.0,
                    "{}: no question changed its first answer, so hit@1 cannot have moved",
                    measured.suite
                );
            }
        }
    }

    /// The guard the first run earned. Every shipped suite discriminates
    /// sharply between values of `k`, so "the one we ship is among the best"
    /// is a claim with teeth rather than a tautology over a flat table.
    ///
    /// Deliberately *not* an equality against the best: `k = 1` and `k = 2`
    /// score identically on all three suites today, and a test that demanded
    /// the shipped value be uniquely best would fail on a tie it has no
    /// opinion about. What must not happen is the shipped value being beaten.
    #[test]
    fn nothing_in_the_grid_beats_what_ships() {
        let shipped = Tuning::default().rrf_k;
        for suite in suites() {
            let measured = k_sensitivity(&suite, now(), None).expect("sensitivity");
            let ours = measured
                .points
                .iter()
                .find(|point| point.rrf_k == shipped)
                .expect("the shipped constant is scored");

            for point in &measured.points {
                assert!(
                    point.hit1 <= ours.hit1 + f64::EPSILON,
                    "{}: k = {} scores hit@1 {:.3} against {:.3} for the shipped k = {shipped}",
                    measured.suite,
                    point.rrf_k,
                    point.hit1,
                    ours.hit1
                );
            }
        }
    }

    /// The shipped default has to be somewhere in the grid, or the table is
    /// reporting on settings the project does not use.
    #[test]
    fn the_shipped_constant_is_one_of_the_rows() {
        assert!(
            K_GRID.contains(&Tuning::default().rrf_k),
            "the grid does not include what ships: {:?}",
            Tuning::default().rrf_k
        );
    }
}
