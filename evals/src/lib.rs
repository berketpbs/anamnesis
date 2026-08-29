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
pub mod corpus;
pub mod run;
pub mod score;
pub mod suite;

pub use ablation::{Ablation, StreamScore, ablate};
pub use corpus::Corpus;
pub use run::{CaseOutcome, Report, run};
pub use score::{CaseScore, mean_reciprocal_rank, recall, score_case};
pub use suite::{Case, FixturePage, Suite, Thresholds};

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

/// The suites built into this binary, by name.
pub fn builtin_suites() -> Vec<(&'static str, &'static str)> {
    vec![("retrieval", RETRIEVAL_SUITE), ("crowded", CROWDED_SUITE)]
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
                "suite {name} scored mrr {:.3} / recall {:.3}, below its own {:.3} / {:.3}; misses: {:?}",
                report.mrr,
                report.recall,
                report.thresholds.min_mrr,
                report.thresholds.min_recall,
                report.misses().map(|case| &case.query).collect::<Vec<_>>(),
            );
        }
    }
}
