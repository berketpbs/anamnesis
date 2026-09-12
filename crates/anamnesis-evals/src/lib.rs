//! Does memory answer the question it was asked?
//!
//! Everything else in this workspace is tested for being correct: a handoff is
//! claimed once, a secret never reaches a page, a sweep drops what it says it
//! will. None of that answers the question the system exists for. Retrieval
//! fuses four streams by reciprocal rank, weights entities by inverse
//! frequency, and multiplies by an authority factor — every one of those
//! numbers was chosen by argument, and until now nothing could say whether
//! changing one made memory better or worse at finding the page somebody
//! needed.
//!
//! A suite is a corpus and the questions asked of it, both checked in as text.
//! It is built through the same calls the server makes and queried through the
//! same call `memory_query` makes, so what it scores is the real path rather
//! than a model of it.
//!
//! What this deliberately does not do:
//!
//! - **It does not run against real memory.** [`Store::query_pages`] records an
//!   access for every page it returns, and the decay sweep reads exactly that
//!   number to decide what to keep. A hundred eval queries would look like a
//!   hundred afternoons of finding those pages useful, and the sweep would
//!   believe it.
//! - **It does not need a model.** The embedding stream is opt-in in
//!   production and absent here, so a score never depends on a 90 MB download
//!   having succeeded.
//! - **It does not grade prose.** Whether a summary reads well is not
//!   something this can decide; whether the page it wrote can be found again
//!   is.
//!
//! [`Store::query_pages`]: anamnesis_store::Store::query_pages

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ablation;
pub mod compare;
pub mod corpus;
pub mod run;
pub mod score;
pub mod sensitivity;
pub mod suite;
pub mod sweep;

pub use ablation::{Ablation, StreamScore, ablate, ablate_with};
pub use compare::{Comparison, Moved, ScoreRow, compare, parse_variant};
pub use corpus::Corpus;
pub use run::{
    CaseOutcome, CategoryScore, Report, Truncated, VectorCoverage, run, run_embedded, run_on,
};
pub use score::{CaseScore, hit_at_one, mean_reciprocal_rank, ndcg_at, recall, score_case};
pub use sensitivity::{K_GRID, KPoint, KSensitivity, k_sensitivity};
pub use suite::{Case, FixturePage, Suite, Thresholds};
pub use sweep::{SuiteScore, SweepPoint, SweepReport, default_grid, sweep};

/// The retrieval suite that ships with anamnesis.
///
/// Embedded rather than read from disk so `anamnesis eval` means the same
/// thing from an installed binary as it does from a checkout.
pub const RETRIEVAL_SUITE: &str = include_str!("../suites/retrieval.toml");

/// The suite retrieval is *not* tuned on.
///
/// Twice the corpus, most questions with a plausible competitor, and half the
/// answers on pages with no authority at all. A knob chosen because it suits
/// ten questions will suit those ten questions; this is the set that says
/// whether it suits anything else.
pub const CROWDED_SUITE: &str = include_str!("../suites/crowded.toml");

/// The suite written to be got wrong.
///
/// A third corpus, sharing no vocabulary with the other two, in which every
/// question has some other page as the better literal match. It was frozen
/// before it was run once, and nothing is ever tuned against it: a case that
/// fails here is a finding about retrieval, not a threshold to lower.
pub const ADVERSARIAL_SUITE: &str = include_str!("../suites/adversarial.toml");

/// The suite whose answers the embedding model never reads.
///
/// Every page in the other three fits inside the default model's window, so
/// none of them can say anything about how a long page is embedded. Here the
/// answer to most questions sits past the first 128 tokens of a long page, and
/// the rest are short pages a long one competes with. Frozen before it was run,
/// like the adversarial suite, and for the same reason.
pub const LONG_SUITE: &str = include_str!("../suites/long.toml");

/// The suites built into this binary, by name.
pub fn builtin_suites() -> Vec<(&'static str, &'static str)> {
    vec![
        ("retrieval", RETRIEVAL_SUITE),
        ("crowded", CROWDED_SUITE),
        ("adversarial", ADVERSARIAL_SUITE),
        ("long", LONG_SUITE),
    ]
}

/// Something an eval could not do.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The suite does not describe a runnable evaluation.
    #[error("{0}")]
    Suite(String),

    /// The corpus could not be built.
    #[error("{0}")]
    Corpus(String),

    /// Storage failed.
    #[error("index error: {0}")]
    Store(#[from] anamnesis_store::StoreError),

    /// The wiki failed.
    #[error("wiki error: {0}")]
    Wiki(#[from] anamnesis_wiki::WikiError),

    /// A core validation rejected part of the suite.
    #[error("{0}")]
    Core(#[from] anamnesis_core::CoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The suite that ships has to be one that loads. It is embedded at
    /// compile time, so a typo in it is otherwise only found by running the
    /// command.
    #[test]
    fn every_builtin_suite_parses() {
        for (name, source) in builtin_suites() {
            let suite = Suite::from_toml(source)
                .unwrap_or_else(|error| panic!("builtin suite {name} does not load: {error}"));
            assert_eq!(suite.name, name);
        }
    }

    /// The claim `long.toml` is built on, checked rather than trusted.
    ///
    /// A deep question — one whose answers are all longer than the window —
    /// must not name anything that appears in the part of the page the model
    /// reads. Otherwise the suite is measuring what it says it is not: a page
    /// whose opening already says the question's words is one the truncated
    /// vector can find, and a change to how long pages are embedded would get
    /// credit for an answer that was reachable all along.
    ///
    /// Counted in words rather than tokens, because this crate has no
    /// tokenizer and should not grow one for a test. A word is never fewer than
    /// one token, so the first 128 words cover at least the first 128 tokens,
    /// and the check is stricter than the window rather than looser. What it
    /// cannot check is meaning: a paraphrase shares no words with its answer
    /// by construction, so for those the guarantee is only that the words are
    /// not there, which is the part a check can make.
    #[test]
    fn the_long_suite_asks_about_what_the_window_cannot_see() {
        /// Tokens the default embedding model reads, `[CLS]` and `[SEP]`
        /// included.
        const WINDOW: usize = 128;
        /// Words a question can share with any English page without saying
        /// what it is about.
        const FUNCTION_WORDS: &[&str] = &[
            "a", "about", "after", "all", "an", "and", "are", "as", "at", "be", "before", "but",
            "by", "can", "did", "do", "does", "every", "for", "from", "has", "have", "how", "i",
            "if", "in", "is", "it", "its", "of", "on", "or", "out", "over", "rather", "than",
            "that", "the", "this", "to", "up", "was", "we", "what", "when", "where", "whose",
            "why", "with",
        ];

        let suite = Suite::from_toml(LONG_SUITE).expect("suite");
        let words = |page: &FixturePage| -> Vec<String> {
            anamnesis_core::embedding::page_text(&page.title, &page.body)
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
                .collect()
        };
        let page = |path: &str| -> &FixturePage {
            suite
                .pages
                .iter()
                .find(|page| page.path == path)
                .expect("the suite validated its own paths")
        };

        let mut deep = 0;
        for case in &suite.cases {
            let answers: Vec<&FixturePage> =
                case.relevant.iter().map(|path| page(path)).collect();
            if !answers.iter().all(|answer| words(answer).len() > WINDOW) {
                continue;
            }
            deep += 1;

            for answer in answers {
                let words = words(answer);
                let opening = &words[..WINDOW];
                for token in anamnesis_core::retrieval::tokenize(&case.query) {
                    if FUNCTION_WORDS.contains(&token.as_str()) {
                        continue;
                    }
                    assert!(
                        !opening.contains(&token),
                        "{:?} asks about {token:?}, which {} says in the part the model reads",
                        case.query,
                        answer.path
                    );
                }
            }
        }

        // Twelve deep questions were written. A page shortened below the
        // window would quietly turn one into a question the check skips, and
        // the suite would go on claiming a depth it no longer has.
        assert_eq!(deep, 12, "the suite was written with twelve deep questions");
    }

    /// And has to clear the bar it sets for itself, or the thresholds are
    /// aspirations rather than a gate.
    #[test]
    fn every_builtin_suite_clears_its_own_bar() {
        let now: jiff::Timestamp = "2026-08-28T09:00:00Z".parse().expect("timestamp");
        for (name, source) in builtin_suites() {
            let suite = Suite::from_toml(source).expect("suite");
            let report = run(&suite, now).expect("run");
            assert!(
                report.passed(),
                "suite {name} scored hit@1 {:.3} / mrr {:.3} / ndcg {:.3} / recall {:.3}, \
                 below its own {:.3} / {:.3} / {:.3} / {:.3}; misses: {:?}",
                report.hit1,
                report.mrr,
                report.ndcg,
                report.recall,
                report.thresholds.min_hit1,
                report.thresholds.min_mrr,
                report.thresholds.min_ndcg,
                report.thresholds.min_recall,
                report.misses().map(|case| &case.query).collect::<Vec<_>>(),
            );
        }
    }
}
