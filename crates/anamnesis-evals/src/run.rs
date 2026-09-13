//! Running a suite and reporting what happened.

use anamnesis_core::embedding::{Embed, Overflow};
use anamnesis_core::page::PagePath;
use anamnesis_core::retrieval::Tuning;
use anamnesis_store::EmbedFault;
use jiff::Timestamp;

use crate::EvalError;
use crate::corpus::Corpus;
use crate::score::{CaseScore, hit_at_one, mean_reciprocal_rank, ndcg_at, recall, score_case};
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
    /// What kind of question it is, or empty in a suite that labels none.
    pub category: String,
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
    /// Share of cases answered in first place.
    pub hit1: f64,
    /// Mean reciprocal rank across the cases.
    pub mrr: f64,
    /// Normalised discounted cumulative gain over the scored window.
    ///
    /// `@limit`, not `@10`: the window is however many results the suite
    /// scores over, and quoting the number without the `k` beside it is how
    /// two projects come to compare figures that were never the same measure.
    pub ndcg: f64,
    /// Share of cases whose answer appeared at all.
    pub recall: f64,
    /// The same four numbers per kind of question, in the suite's own order.
    ///
    /// Empty for a suite that labels nothing.
    pub by_category: Vec<CategoryScore>,
    /// The bar the suite set for itself.
    pub thresholds: crate::suite::Thresholds,
    /// How much of the corpus the vector stream actually read, when there was
    /// one. `None` on a run without an embedder.
    pub vectors: Option<VectorCoverage>,
}

/// How much of a corpus its vectors stand for.
///
/// On the report because a vector score is a claim about two things at once —
/// the stream, and what the stream was given — and a suite whose pages all fit
/// the model's window cannot say anything about pages that do not. When this
/// was added, every page in the three suites that then shipped fit, while 43 of
/// the 49 pages in this project's own wiki did not. A change to how long pages
/// are embedded would have scored identically before and after on every one of
/// them, and read as a change that did nothing — which is why `long.toml`
/// exists.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorCoverage {
    /// The model that embedded the corpus.
    pub model: String,
    /// Pages in the corpus.
    pub pages: usize,
    /// Pages that got no vector at all.
    pub failed: usize,
    /// Pages whose vector stands for only part of them, least read first.
    pub truncated: Vec<Truncated>,
    /// Pages whose abstract has a vector of its own.
    ///
    /// Zero on a suite whose pages carry no abstract, where `--compare
    /// abstracts=1` has nothing to rank and scores what ships — which must not
    /// read as the stream having been tried and found to do nothing.
    pub abstracts: usize,
}

/// One page the model read only the start of.
#[derive(Debug, Clone, PartialEq)]
pub struct Truncated {
    /// The page, as the suite names it.
    pub path: String,
    /// What the page came to, and what the model read.
    pub overflow: Overflow,
    /// How many sections the page was also embedded in, all of them read
    /// whole. Zero where it could not be, which leaves the page with nothing
    /// but its opening under either setting of `vector_sections` — and a
    /// comparison of that setting that moved nothing would otherwise be
    /// indistinguishable from one that had nothing to move.
    pub sections: usize,
}

impl VectorCoverage {
    /// Pages the vector stream saw whole.
    pub fn whole(&self) -> usize {
        self.pages
            .saturating_sub(self.failed)
            .saturating_sub(self.truncated.len())
    }

    /// Whether any page in the corpus is longer than the model reads.
    ///
    /// When this is false the suite is silent on long pages by construction,
    /// whatever it scores, and a comparison that changes how they are embedded
    /// measures nothing.
    pub fn reaches_the_window(&self) -> bool {
        !self.truncated.is_empty()
    }

    /// Truncated pages that were also embedded in sections.
    pub fn sectioned(&self) -> usize {
        self.truncated
            .iter()
            .filter(|page| page.sections > 0)
            .count()
    }

    /// Read back from the index a corpus was built into.
    ///
    /// From the complaint rows the write path records, rather than by asking
    /// the embedder again: what is being reported is what indexing did, and a
    /// second count taken here could disagree with it without anyone knowing
    /// which one retrieval ran on.
    fn of(corpus: &Corpus, model: &str, pages: usize) -> Result<Self, EvalError> {
        let mut failed = 0;
        let mut truncated = Vec::new();
        for complaint in corpus.store.embed_failures(corpus.project_id)? {
            if complaint.model != model {
                continue;
            }
            match (complaint.kind, complaint.tokens, complaint.budget) {
                (EmbedFault::Truncated, Some(tokens), Some(budget)) => truncated.push(Truncated {
                    path: complaint.path.as_str().to_owned(),
                    overflow: Overflow { tokens, budget },
                    sections: complaint.sections,
                }),
                // A truncation row without its numbers still says the vector
                // is partial; it is counted as a failure rather than dropped,
                // since "whole" is the one thing it certainly is not.
                _ => failed += 1,
            }
        }
        truncated.sort_by(|a, b| {
            a.overflow
                .covered()
                .total_cmp(&b.overflow.covered())
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(Self {
            model: model.to_owned(),
            pages,
            failed,
            truncated,
            abstracts: corpus
                .store
                .abstract_embedding_count(corpus.project_id, model)?,
        })
    }
}

/// One kind of question, scored on its own.
///
/// The reason a total is not enough: a change that teaches retrieval to match
/// a paraphrase can cost it a bare keyword, and a single mean over both reports
/// that nothing much happened. Published beside the total so a trade has to be
/// stated rather than netted out.
#[derive(Debug, Clone)]
pub struct CategoryScore {
    /// The label, as the suite declared it.
    pub name: String,
    /// How many questions carry it.
    ///
    /// Read this before the rates. Over four questions a rate moves in
    /// quarters, and a quarter is not a finding.
    pub cases: usize,
    /// Share answered in first place.
    pub hit1: f64,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// Normalised discounted cumulative gain over the scored window.
    pub ndcg: f64,
    /// Share whose answer appeared at all.
    pub recall: f64,
}

impl Report {
    /// Whether the suite cleared its own bar.
    ///
    /// A suite that declares no thresholds always passes; declaring the
    /// numbers is how a suite opts into being a gate.
    pub fn passed(&self) -> bool {
        self.mrr >= self.thresholds.min_mrr
            && self.recall >= self.thresholds.min_recall
            && self.hit1 >= self.thresholds.min_hit1
            && self.ndcg >= self.thresholds.min_ndcg
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
    run_on(&corpus, suite, now, &Tuning::default(), None)
}

/// Score a suite with the embedding stream switched on.
///
/// Both halves matter and both are the caller's embedder: the corpus is
/// embedded page by page, and every question is embedded with the same model,
/// because a query vector from one model and a page vector from another are
/// not comparable and nothing downstream would say so.
pub fn run_embedded(
    suite: &Suite,
    now: Timestamp,
    embedder: &dyn Embed,
) -> Result<Report, EvalError> {
    let corpus = Corpus::build_with(suite, now, Some(embedder))?;
    run_on(&corpus, suite, now, &Tuning::default(), Some(embedder))
}

/// Score a suite against a corpus that is already built.
///
/// Separate from [`run`] because a sweep asks the same questions of the same
/// pages sixty times over, and building the corpus is the expensive half: every
/// page is a file written into a git repository and committed.
pub fn run_on(
    corpus: &Corpus,
    suite: &Suite,
    now: Timestamp,
    tuning: &Tuning,
    embedder: Option<&dyn Embed>,
) -> Result<Report, EvalError> {
    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        cases.push(run_case(corpus, case, suite.limit, now, tuning, embedder)?);
    }

    let scores: Vec<CaseScore> = cases.iter().map(|case| case.score.clone()).collect();
    let by_category = score_by_category(&suite.categories, &cases, suite.limit);
    let vectors = embedder
        .map(|embedder| VectorCoverage::of(corpus, embedder.model(), suite.pages.len()))
        .transpose()?;

    Ok(Report {
        name: suite.name.clone(),
        description: suite.description.clone(),
        limit: suite.limit,
        pages: suite.pages.len(),
        hit1: hit_at_one(&scores),
        mrr: mean_reciprocal_rank(&scores),
        ndcg: ndcg_at(&scores, suite.limit),
        recall: recall(&scores),
        by_category,
        thresholds: suite.thresholds,
        vectors,
        cases,
    })
}

/// Slice the outcomes by the labels the suite declared, in the order it
/// declared them.
///
/// The suite's order rather than a sort: whoever wrote the list put the kinds
/// in the order they wanted them read, and alphabetising it would put
/// `keyword` above `paraphrase` for reasons that are about the alphabet.
fn score_by_category(
    categories: &[String],
    cases: &[CaseOutcome],
    limit: usize,
) -> Vec<CategoryScore> {
    categories
        .iter()
        .map(|name| {
            let scores: Vec<CaseScore> = cases
                .iter()
                .filter(|case| &case.category == name)
                .map(|case| case.score.clone())
                .collect();
            CategoryScore {
                name: name.clone(),
                cases: scores.len(),
                hit1: hit_at_one(&scores),
                mrr: mean_reciprocal_rank(&scores),
                ndcg: ndcg_at(&scores, limit),
                recall: recall(&scores),
            }
        })
        .collect()
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
    tuning: &Tuning,
    embedder: Option<&dyn Embed>,
) -> Result<CaseOutcome, EvalError> {
    // A question that cannot be embedded is asked without a vector rather than
    // failing the case: the other three streams are what most of this measures,
    // and a suite that refused to score because one model call failed would be
    // measuring the model.
    let vector = embedder.and_then(|embedder| {
        embedder
            .embed(&case.query)
            .ok()
            .map(|vector| (embedder.model().to_owned(), vector))
    });

    let hits = corpus.store.query_pages_with(
        corpus.project_id,
        &case.query,
        limit,
        now,
        vector
            .as_ref()
            .map(|(model, vector)| (model.as_str(), vector.as_slice())),
        tuning,
    )?;

    let returned: Vec<String> = hits
        .iter()
        .map(|hit| hit.path.as_str().to_owned())
        .collect();

    Ok(CaseOutcome {
        query: case.query.clone(),
        category: case.category.clone(),
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

    /// A measure that no gate reads is a decoration. This is the whole reason
    /// the two new numbers are on the report rather than only in the printing:
    /// a suite can be held to first place, and fail there while its recall and
    /// its mean are both untouched.
    ///
    /// Constructed rather than provoked, because this two-page corpus answers
    /// both its questions outright and there is no honest way to make it miss
    /// one without changing what it measures.
    #[test]
    fn a_bar_on_first_place_can_fail_a_suite_the_older_numbers_pass() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let mut report = run(&suite, now()).expect("run");
        assert!(report.passed(), "the corpus answers itself");
        assert_eq!(report.hit1, 1.0);
        assert_eq!(report.ndcg, 1.0);

        report.hit1 = 0.5;
        report.thresholds.min_hit1 = 1.0;
        assert!(!report.passed(), "recall and mrr still pass; this must not");

        report.hit1 = 1.0;
        report.ndcg = 0.5;
        report.thresholds.min_ndcg = 1.0;
        assert!(!report.passed());
    }

    /// The window the gain is discounted over is the suite's, not a constant.
    /// A suite scoring over three and one scoring over ten report different
    /// measures, and the report has to carry which one it took.
    #[test]
    fn the_gain_is_taken_over_the_window_the_suite_scores() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run(&suite, now()).expect("run");
        assert_eq!(report.limit, suite.limit);
    }

    /// The thing a per-category table is for: one kind of question failing
    /// while the total stays respectable. Here one of two categories misses
    /// entirely, the suite still reports 0.500 overall, and the row says which
    /// half it was.
    #[test]
    fn a_category_that_fails_is_named_while_the_total_stays_respectable() {
        let source = SUITE
            .replace(
                "limit = 3",
                "limit = 3\ncategories = [\"keyword\", \"paraphrase\"]",
            )
            .replace(
                "query = \"sqlite\"",
                "query = \"sqlite\"\ncategory = \"keyword\"",
            )
            .replace(
                "query = \"byte order mark\"",
                "query = \"kubernetes ingress\"\ncategory = \"paraphrase\"",
            );
        let suite = Suite::from_toml(&source).expect("suite");
        let report = run(&suite, now()).expect("run");

        assert_eq!(report.recall, 0.5);
        assert_eq!(report.by_category.len(), 2);

        let keyword = &report.by_category[0];
        assert_eq!(keyword.name, "keyword");
        assert_eq!(keyword.cases, 1);
        assert_eq!(keyword.hit1, 1.0);

        let paraphrase = &report.by_category[1];
        assert_eq!(paraphrase.name, "paraphrase");
        assert_eq!(paraphrase.cases, 1);
        assert_eq!(paraphrase.recall, 0.0, "the half that failed");
    }

    /// The order is the suite's, not the alphabet's: whoever wrote the list put
    /// the kinds in the order they wanted them read.
    #[test]
    fn categories_are_reported_in_the_order_the_suite_declares_them() {
        let source = SUITE
            .replace(
                "limit = 3",
                "limit = 3\ncategories = [\"paraphrase\", \"keyword\"]",
            )
            .replace(
                "query = \"sqlite\"",
                "query = \"sqlite\"\ncategory = \"keyword\"",
            )
            .replace(
                "query = \"byte order mark\"",
                "query = \"byte order mark\"\ncategory = \"paraphrase\"",
            );
        let suite = Suite::from_toml(&source).expect("suite");
        let report = run(&suite, now()).expect("run");

        let names: Vec<&str> = report
            .by_category
            .iter()
            .map(|category| category.name.as_str())
            .collect();
        assert_eq!(names, ["paraphrase", "keyword"]);
    }

    /// A suite that labels nothing reports nothing, rather than one row called
    /// "uncategorised" that is the total written twice.
    #[test]
    fn an_unlabelled_suite_reports_no_categories() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run(&suite, now()).expect("run");
        assert!(report.by_category.is_empty());
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

    /// An embedder that reads a fixed number of words and knows it.
    struct Narrow {
        budget: usize,
    }

    impl Embed for Narrow {
        fn model(&self) -> &str {
            "narrow-1"
        }
        fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            // Direction is irrelevant here; only what indexing records is.
            Ok(vec![text.len() as f32, 1.0])
        }
        fn overflow(&self, text: &str) -> Option<Overflow> {
            let tokens = text.split_whitespace().count();
            (tokens > self.budget).then_some(Overflow {
                tokens,
                budget: self.budget,
            })
        }
    }

    /// No embedder, no claim about vectors — rather than a row of zeroes that
    /// reads as a stream which saw nothing.
    #[test]
    fn a_run_without_vectors_says_nothing_about_them() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run(&suite, now()).expect("run");
        assert_eq!(report.vectors, None);
    }

    /// The thing the field is for. Both pages here fit a generous window, and
    /// the report has to be able to say so: a suite like that is silent on long
    /// pages however well it scores.
    #[test]
    fn a_corpus_that_fits_the_window_is_reported_as_not_reaching_it() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run_embedded(&suite, now(), &Narrow { budget: 1000 }).expect("run");

        let vectors = report.vectors.expect("an embedded run reports coverage");
        assert_eq!(vectors.model, "narrow-1");
        assert_eq!(vectors.pages, 2);
        assert_eq!(vectors.whole(), 2);
        assert!(!vectors.reaches_the_window());
    }

    /// And the other way: a page longer than the model reads is named, with
    /// how much of it the vector stands for, and the least-read page comes
    /// first because that is the question somebody reading this has.
    #[test]
    fn a_page_longer_than_the_window_is_named_least_read_first() {
        // Title and body are embedded together, so the SQLite page comes to
        // thirteen words and the Windows page to ten. A window of ten reads
        // one of them whole.
        let suite = Suite::from_toml(SUITE).expect("suite");
        let report = run_embedded(&suite, now(), &Narrow { budget: 10 }).expect("run");

        let vectors = report.vectors.expect("coverage");
        assert!(vectors.reaches_the_window());
        assert_eq!(vectors.failed, 0);
        assert_eq!(vectors.whole(), 1, "{vectors:?}");
        assert_eq!(vectors.truncated.len(), 1, "{vectors:?}");

        let least = &vectors.truncated[0];
        assert_eq!(least.path, "decisions/0001-sqlite.md");
        assert_eq!(least.overflow.tokens, 13);
        assert_eq!(least.overflow.budget, 10);
        assert!(least.sections > 1, "{least:?}");
        assert_eq!(vectors.sectioned(), 1);

        // Lower the window until both overflow, and the order has to follow
        // coverage rather than the order the suite lists them in.
        let report = run_embedded(&suite, now(), &Narrow { budget: 5 }).expect("run");
        let vectors = report.vectors.expect("coverage");
        let order: Vec<&str> = vectors
            .truncated
            .iter()
            .map(|page| page.path.as_str())
            .collect();
        assert_eq!(order, ["decisions/0001-sqlite.md", "notes/windows.md"]);

        // Now swap which page is longer, and the order has to swap with it.
        let swapped = SUITE.replace(
            "body = \"PowerShell prepends a byte order mark when piping.\"",
            "body = \"PowerShell prepends a byte order mark when piping a string to a file on disk.\"",
        );
        let suite = Suite::from_toml(&swapped).expect("suite");
        let report = run_embedded(&suite, now(), &Narrow { budget: 5 }).expect("run");
        let vectors = report.vectors.expect("coverage");
        assert_eq!(vectors.truncated[0].path, "notes/windows.md", "{vectors:?}");
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
            category: String::new(),
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
