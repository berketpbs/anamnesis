//! Two tunings, the same questions, and what moved between them.
//!
//! `docs/DIRECTION.md` adopts a rule — *a retrieval change without a paired
//! measurement does not land* — and until now there was nothing to measure one
//! with. [`mod@crate::sweep`] ranks sixty settings by a mean, and a mean is
//! exactly where a trade hides: a change that lifts three questions and drops
//! two reports as a small gain, and reads identically to one that lifted five
//! and dropped none. Those are not the same change and only one of them should
//! land.
//!
//! So the output here is not a pair of numbers. It is **which questions got
//! better and which got worse, by name**, with the aggregate printed beside
//! them rather than instead of them. A regression on one question is a fact
//! about that question; averaging it away is how a corpus stops being able to
//! object.
//!
//! Nothing here reads configuration, and that is deliberate: [`Tuning`]'s own
//! documentation says nothing loads it from a file, because a knob set per
//! project and measured by nobody is the class of setting this codebase keeps
//! deleting. A variant is expressed for the length of one command, measured,
//! and then either becomes a new default in code or does not.

use anamnesis_core::retrieval::Tuning;
use jiff::Timestamp;

use crate::EvalError;
use crate::corpus::Corpus;
use crate::run::{Report, run_on};
use crate::score::{CaseScore, hit_at_one, mean_reciprocal_rank, ndcg_at, recall};
use crate::suite::Suite;

use anamnesis_core::embedding::Embed;

/// One tuning's four measures over a suite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreRow {
    /// Share of questions answered first.
    pub hit1: f64,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// NDCG over the suite's scored window.
    pub ndcg: f64,
    /// Share of relevant pages returned.
    pub recall: f64,
}

impl ScoreRow {
    fn of(report: &Report) -> Self {
        let scores: Vec<CaseScore> = report.cases.iter().map(|case| case.score.clone()).collect();
        Self {
            hit1: hit_at_one(&scores),
            mrr: mean_reciprocal_rank(&scores),
            ndcg: ndcg_at(&scores, report.limit),
            recall: recall(&scores),
        }
    }
}

/// One question that answered differently under the variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The question, as the suite asks it.
    pub query: String,
    /// The label the suite gave it, so a trade between kinds of asking shows.
    pub category: String,
    /// Rank of the best relevant page under what ships. `None` is "nothing
    /// relevant came back", which is worse than any rank rather than better
    /// than all of them.
    pub before: Option<usize>,
    /// The same under the variant.
    pub after: Option<usize>,
}

/// What one variant did to one suite, question by question.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// Which suite.
    pub suite: String,
    /// How many pages it holds.
    pub pages: usize,
    /// How many questions were asked.
    pub cases: usize,
    /// What the variant changed, spelled the way it was asked for.
    pub variant: String,
    /// Scores under [`Tuning::default`].
    pub baseline: ScoreRow,
    /// Scores under the variant.
    pub after: ScoreRow,
    /// Questions whose best relevant answer moved up.
    pub improved: Vec<Moved>,
    /// Questions whose best relevant answer moved down, or stopped coming back.
    ///
    /// The list the rule exists for. A change with anything in here has to
    /// argue for itself rather than point at a mean.
    pub regressed: Vec<Moved>,
    /// Questions that answered at exactly the same rank.
    pub unchanged: usize,
}

impl Comparison {
    /// Whether the variant cost nothing on any question.
    ///
    /// Not "is it better" — a variant that helps nothing and hurts nothing is
    /// clean by this measure and is also pointless. It answers the narrower
    /// question the rule is about: is there a trade here that a mean would
    /// hide?
    pub fn clean(&self) -> bool {
        self.regressed.is_empty()
    }

    /// Change in hit@1, variant minus baseline.
    pub fn hit1_delta(&self) -> f64 {
        self.after.hit1 - self.baseline.hit1
    }

    /// Change in mean reciprocal rank, variant minus baseline.
    pub fn mrr_delta(&self) -> f64 {
        self.after.mrr - self.baseline.mrr
    }
}

/// Score one suite under what ships and under `variant`, and pair the results.
///
/// The corpus is built once and both tunings are run against it, because two
/// corpora would differ for reasons that are not the tuning.
pub fn compare(
    suite: &Suite,
    now: Timestamp,
    variant: &Tuning,
    described: &str,
    embedder: Option<&dyn Embed>,
) -> Result<Comparison, EvalError> {
    let corpus = Corpus::build_with(suite, now, embedder)?;
    let baseline = run_on(&corpus, suite, now, &Tuning::default(), embedder)?;
    let after = run_on(&corpus, suite, now, variant, embedder)?;

    let mut improved = Vec::new();
    let mut regressed = Vec::new();
    let mut unchanged = 0usize;

    for (index, case) in baseline.cases.iter().enumerate() {
        let Some(other) = after
            .cases
            .get(index)
            .filter(|other| other.query == case.query)
        else {
            continue;
        };
        let before = case.score.rank;
        let now_at = other.score.rank;
        if before == now_at {
            unchanged += 1;
            continue;
        }
        let moved = Moved {
            query: case.query.clone(),
            category: case.category.clone(),
            before,
            after: now_at,
        };
        if better(now_at, before) {
            improved.push(moved);
        } else {
            regressed.push(moved);
        }
    }

    Ok(Comparison {
        suite: suite.name.clone(),
        pages: suite.pages.len(),
        cases: suite.cases.len(),
        variant: described.to_owned(),
        baseline: ScoreRow::of(&baseline),
        after: ScoreRow::of(&after),
        improved,
        regressed,
        unchanged,
    })
}

/// Whether `candidate` is a better outcome than `incumbent`.
///
/// `None` means nothing relevant came back, and it is the worst outcome rather
/// than the best: ordering `Option<usize>` the way Rust does would put "not
/// found" above rank one and silently record every disappearance as a win.
fn better(candidate: Option<usize>, incumbent: Option<usize>) -> bool {
    match (candidate, incumbent) {
        (Some(new), Some(old)) => new < old,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Read a variant off the command line: `rrf_k=5,links=0.5`.
///
/// Deliberately not a file. See the module note: a tuning that can be written
/// down somewhere durable is a tuning somebody sets and nobody measures.
pub fn parse_variant(spec: &str) -> Result<(Tuning, String), EvalError> {
    let mut tuning = Tuning::default();
    let mut named = Vec::new();

    for clause in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, value) = clause.split_once('=').ok_or_else(|| {
            EvalError::Corpus(format!(
                "{clause:?} is not name=value; try rrf_k=5 or links=0.5"
            ))
        })?;
        let name = name.trim();
        let value = value.trim();

        // `candidates` is the one integer among them, and parsing it as a
        // float and casting would quietly accept `candidates=30.7`.
        if name == "candidates" {
            tuning.candidates = value.parse::<usize>().map_err(|_| {
                EvalError::Corpus(format!("candidates wants a whole number, got {value:?}"))
            })?;
            named.push(format!("candidates={value}"));
            continue;
        }

        let number: f64 = value
            .parse()
            .map_err(|_| EvalError::Corpus(format!("{name} wants a number, got {value:?}")))?;
        match name {
            "rrf_k" => tuning.rrf_k = number,
            "fts" => tuning.fts = number,
            "entity" => tuning.entity = number,
            "links" => tuning.links = number,
            "vectors" => tuning.vectors = number,
            "authority_exponent" => tuning.authority_exponent = number,
            "entity_coverage" => tuning.entity_coverage = number,
            other => {
                return Err(EvalError::Corpus(format!(
                    "no tuning called {other:?}. Try one of: rrf_k, fts, entity, links, \
                     vectors, authority_exponent, entity_coverage, candidates"
                )));
            }
        }
        named.push(format!("{name}={value}"));
    }

    if named.is_empty() {
        return Err(EvalError::Corpus(
            "--compare needs at least one name=value, or it would measure nothing against itself"
                .to_owned(),
        ));
    }
    Ok((tuning, named.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite() -> Suite {
        Suite::from_toml(crate::RETRIEVAL_SUITE).expect("the shipped suite parses")
    }

    fn now() -> Timestamp {
        "2026-01-01T00:00:00Z".parse().expect("timestamp")
    }

    #[test]
    fn a_variant_identical_to_what_ships_moves_nothing() {
        let measured = compare(&suite(), now(), &Tuning::default(), "none", None).expect("compare");

        assert!(measured.improved.is_empty());
        assert!(measured.regressed.is_empty());
        assert_eq!(measured.unchanged, measured.cases);
        assert!(measured.clean());
        assert_eq!(measured.hit1_delta(), 0.0);
    }

    /// The measurement the rule is for. `k = 60` is known to cost this suite
    /// hit@1, and a comparison that could not name which questions paid would
    /// be the mean the rule was written against.
    #[test]
    fn a_variant_that_costs_something_names_what_it_cost() {
        let variant = Tuning {
            rrf_k: 60.0,
            ..Tuning::default()
        };
        let measured = compare(&suite(), now(), &variant, "rrf_k=60", None).expect("compare");

        assert!(!measured.clean(), "k = 60 is known to cost this suite");
        assert!(!measured.regressed.is_empty());
        assert!(measured.hit1_delta() < 0.0);
        for moved in &measured.regressed {
            assert!(!moved.query.is_empty());
            assert!(
                !better(moved.after, moved.before),
                "{moved:?} was filed as a regression but improved"
            );
        }
    }

    /// Every question is accounted for exactly once, or the report is a
    /// summary with a hole in it.
    #[test]
    fn the_three_lists_add_up_to_the_suite() {
        let variant = Tuning {
            rrf_k: 30.0,
            ..Tuning::default()
        };
        let measured = compare(&suite(), now(), &variant, "rrf_k=30", None).expect("compare");

        assert_eq!(
            measured.improved.len() + measured.regressed.len() + measured.unchanged,
            measured.cases
        );
    }

    /// Nothing coming back is the worst outcome, not the best. Rust orders
    /// `None` below `Some`, so a comparison that leaned on that would record
    /// every disappearance as a win.
    #[test]
    fn losing_the_answer_entirely_is_a_regression() {
        assert!(!better(None, Some(1)));
        assert!(!better(None, None));
        assert!(better(Some(9), None));
        assert!(better(Some(1), Some(2)));
        assert!(!better(Some(2), Some(1)));
    }

    #[test]
    fn a_variant_is_read_off_the_command_line() {
        let (tuning, described) = parse_variant("rrf_k=5, links=0.5,candidates=10").expect("parse");
        assert_eq!(tuning.rrf_k, 5.0);
        assert_eq!(tuning.links, 0.5);
        assert_eq!(tuning.candidates, 10);
        // Untouched knobs keep what ships.
        assert_eq!(tuning.fts, Tuning::default().fts);
        assert_eq!(described, "rrf_k=5 links=0.5 candidates=10");
    }

    #[test]
    fn a_misspelled_knob_is_refused_with_the_list() {
        let error = parse_variant("rrf-k=5").expect_err("no such knob");
        let message = error.to_string();
        assert!(message.contains("rrf_k"), "{message}");
    }

    /// `candidates=30.7` parsed as a float and cast would silently become 30.
    #[test]
    fn a_whole_number_knob_refuses_a_fraction() {
        parse_variant("candidates=30.7").expect_err("candidates is a count");
    }

    #[test]
    fn an_empty_variant_is_refused_rather_than_compared_to_itself() {
        parse_variant("").expect_err("nothing to vary");
        parse_variant("rrf_k").expect_err("no value");
    }
}
