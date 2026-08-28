//! Running a suite and reporting what happened.

use anamnesis_core::page::PagePath;
use jiff::Timestamp;

use crate::EvalError;
use crate::corpus::Corpus;
use crate::score::{CaseScore, mean_reciprocal_rank, recall, score_case};
use crate::suite::{Case, Suite};

/// Rank at which an answer stops being one the reader is likely to see.
///
/// Three, because the agent that asked reads from the top and stops. Not a
/// threshold anything fails on — just where the report starts pointing.
pub const LOW_RANK: usize = 3;

/// What one case did.
#[derive(Debug, Clone)]
pub struct CaseOutcome {
    /// The question as it was asked.
    pub query: String,
    /// Why the case is in the suite.
    pub note: String,
    /// Pages that would have answered it.
    pub relevant: Vec<String>,
    /// What retrieval returned, best first.
    pub returned: Vec<String>,
    /// How it scored.
    pub score: CaseScore,
}

/// What a whole suite did.
#[derive(Debug, Clone)]
pub struct Report {
    /// Which suite this is.
    pub name: String,
    /// What it was trying to find out.
    pub description: String,
    /// How many results each case was scored over.
    pub limit: usize,
    /// How many pages the questions were asked of.
    pub pages: usize,
    /// Every case, in the order the suite lists them.
    pub cases: Vec<CaseOutcome>,
    /// Mean reciprocal rank across the cases.
    pub mrr: f64,
    /// Share of cases whose answer appeared at all.
    pub recall: f64,
    /// The bar the suite set for itself.
    pub thresholds: crate::suite::Thresholds,
}

impl Report {
    /// Whether the suite cleared its own bar.
    ///
    /// A suite that declares no thresholds always passes; declaring the
    /// numbers is how a suite opts into being a gate.
    pub fn passed(&self) -> bool {
        self.mrr >= self.thresholds.min_mrr && self.recall >= self.thresholds.min_recall
    }

    /// The cases that returned nothing relevant, which are the ones worth
    /// reading first.
    pub fn misses(&self) -> impl Iterator<Item = &CaseOutcome> {
        self.cases.iter().filter(|case| !case.score.found())
    }

    /// Cases whose answer came back, but far enough down to be a near miss.
    ///
    /// Worth its own list because the averages hide it: a suite can hold
    /// perfect recall while an answer slides from first place to fifth, and
    /// nobody scrolls to fifth. `LOW_RANK` is where "found it" stops meaning
    /// "was given it".
    pub fn ranked_low(&self) -> impl Iterator<Item = &CaseOutcome> {
        self.cases
            .iter()
            .filter(|case| case.score.rank.is_some_and(|rank| rank >= LOW_RANK))
    }
}

/// Build the corpus, ask every question, and score the answers.
///
/// `now` is passed in rather than read: two runs of the same suite have to
/// produce the same report, and page freshness is an input to nothing here
/// only because the clock is held still.
pub fn run(suite: &Suite, now: Timestamp) -> Result<Report, EvalError> {
    let corpus = Corpus::build(suite, now)?;

    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        cases.push(run_case(&corpus, case, suite.limit, now)?);
    }

    let scores: Vec<CaseScore> = cases.iter().map(|case| case.score.clone()).collect();

    Ok(Report {
        name: suite.name.clone(),
        description: suite.description.clone(),
        limit: suite.limit,
        pages: suite.pages.len(),
        mrr: mean_reciprocal_rank(&scores),
        recall: recall(&scores),
        thresholds: suite.thresholds,
        cases,
    })
}

/// Ask one question of a built corpus.
///
/// The query goes through `query_pages` — the same call `memory_query` and
/// `anamnesis search` make. Anything this eval measures that the real path
/// does not do would be a measurement of the eval.
fn run_case(
    corpus: &Corpus,
    case: &Case,
    limit: usize,
    now: Timestamp,
) -> Result<CaseOutcome, EvalError> {
    let hits = corpus
        .store
        .query_pages(corpus.project_id, &case.query, limit, now, None)?;

    let returned: Vec<String> = hits
        .iter()
        .map(|hit| hit.path.as_str().to_owned())
        .collect();

    Ok(CaseOutcome {
        query: case.query.clone(),
        note: case.note.clone(),
        relevant: case.relevant.clone(),
        score: score_case(&returned, &case.relevant),
        returned,
    })
}

/// Whether a path names a page the suite expects.
///
/// Kept beside the runner because it is the one place a stored path and an
/// authored one are compared, and they are normalised differently: the suite
/// writes what a person would type, the index stores what [`PagePath`]
/// validated.
pub fn normalise(path: &str) -> Result<String, EvalError> {
    Ok(PagePath::parse(path)?.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUITE: &str = r#"
name = "run-test"
description = "a corpus small enough to reason about"
limit = 3

[thresholds]
min_mrr = 0.5
min_recall = 1.0

[[page]]
path = "decisions/0001-sqlite.md"
title = "Why SQLite"
tier = "semantic"
entities = ["SQLite"]
body = "The index is a single file, which is why SQLite won."

[[page]]
path = "notes/windows.md"
title = "Windows notes"
body = "PowerShell prepends a byte order mark when piping."

[[case]]
query = "sqlite"
relevant = ["decisions/0001-sqlite.md"]

[[case]]
query = "byte order mark"
relevant = ["notes/windows.md"]
"#;

    fn now() -> Timestamp {
        "2026-08-28T09:00:00Z".parse().expect("timestamp")
    }

    #[test]
    fn a_suite_that_answers_its_own_questions_passes_its_bar() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run(&suite, now()).expect("run");

        assert_eq!(report.cases.len(), 2);
        assert_eq!(report.recall, 1.0);
        assert!(report.passed(), "{report:?}");
        assert_eq!(report.misses().count(), 0);
    }

    /// The report has to be able to fail, and a question the corpus cannot
    /// answer is how it does: a miss scores zero and drags both figures down.
    #[test]
    fn an_unanswerable_question_fails_the_suite() {
        let source = SUITE.replace("query = \"sqlite\"", "query = \"kubernetes ingress\"");
        let suite = Suite::from_toml(&source).expect("suite");
        let report = run(&suite, now()).expect("run");

        assert_eq!(report.recall, 0.5);
        assert!(!report.passed());
        assert_eq!(report.misses().count(), 1);
        assert_eq!(
            report.misses().next().expect("a miss").query,
            "kubernetes ingress"
        );
    }

    /// Two runs of one suite have to agree, or no number it prints means
    /// anything from one day to the next.
    #[test]
    fn the_same_suite_scores_the_same_twice() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let first = run(&suite, now()).expect("run");
        let second = run(&suite, now()).expect("run");

        assert_eq!(first.mrr, second.mrr);
        assert_eq!(first.recall, second.recall);
        let ranks = |report: &Report| -> Vec<Option<usize>> {
            report.cases.iter().map(|case| case.score.rank).collect()
        };
        assert_eq!(ranks(&first), ranks(&second));
    }

    /// Perfect recall with the answer at the bottom of the page is the result
    /// most likely to be read as "fine", which is why it gets its own list.
    #[test]
    fn an_answer_that_only_just_made_it_is_pointed_at() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run(&suite, now()).expect("run");
        // This corpus answers both questions outright.
        assert_eq!(report.ranked_low().count(), 0);

        let outcome = CaseOutcome {
            query: "buried".to_owned(),
            note: String::new(),
            relevant: vec!["a.md".to_owned()],
            returned: vec!["x.md".into(), "y.md".into(), "a.md".into()],
            score: crate::score::score_case(
                &["x.md".to_owned(), "y.md".to_owned(), "a.md".to_owned()],
                &["a.md".to_owned()],
            ),
        };
        let mut report = report;
        report.cases.push(outcome);
        assert_eq!(report.misses().count(), 0, "it was found");
        assert_eq!(report.ranked_low().count(), 1, "but only just");
    }
}
