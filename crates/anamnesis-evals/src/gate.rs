//! Does recall keep quiet when it has nothing to say?
//!
//! The rest of this crate asks whether a question finds its page. Recall at
//! prompt time has a second question, and it is the harder one: a block
//! injected on every prompt whether or not it has anything to say teaches an
//! agent to skip it, so what matters as much as finding the page is **not
//! finding one** when the project has none.
//!
//! That half is usually measured with labels — somebody reads each prompt and
//! decides whether the project had anything to say about it — which is slow,
//! and is one person's judgement. There is a way round it that needs no
//! labels at all. The suites here are four corpora about four unrelated
//! systems. A question written for one of them, asked of another, has no
//! answer there by construction, so **anything recall hands back for it is a
//! false alarm**. Asked of its own corpus, the same question has a known
//! answer. Every suite's questions against every corpus gives both numbers at
//! once:
//!
//! - **home**: of a corpus's own questions, how many got a block, and how many
//!   of those blocks led with a page the question names as its answer;
//! - **cross**: of every other suite's questions, how many got a block at all.
//!
//! Recall here is [`anamnesis_store::Store::pages_named_by`], the gate that
//! needs no model, so this runs in CI with nothing downloaded. Like the rest of
//! this crate it runs on throwaway corpora, never on real memory, and records
//! nothing.

use anamnesis_store::Naming;
use jiff::Timestamp;

use crate::EvalError;
use crate::corpus::Corpus;
use crate::run::normalise;
use crate::suite::Suite;

/// How many pages a recall block offers, as `[recall] pages` does by default.
const OFFERED: usize = 3;

/// What recall did on one corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRow {
    /// The suite whose corpus was asked.
    pub corpus: String,
    /// Its own questions.
    pub home: usize,
    /// Of those, how many got a block.
    pub home_fired: usize,
    /// Of those, how many blocks led with a page the question names.
    pub home_right: usize,
    /// Other suites' questions, which this corpus cannot answer.
    pub cross: usize,
    /// Of those, how many got a block anyway.
    pub cross_fired: usize,
}

impl GateRow {
    /// Share of the corpus's own questions that got a block.
    pub fn home_rate(&self) -> f64 {
        ratio(self.home_fired, self.home)
    }

    /// Share of blocks on the corpus's own questions that led with an answer.
    pub fn precision(&self) -> f64 {
        ratio(self.home_right, self.home_fired)
    }

    /// Share of unanswerable questions that got a block: every one is a false
    /// alarm.
    pub fn cross_rate(&self) -> f64 {
        ratio(self.cross_fired, self.cross)
    }
}

fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Ask every suite's questions of every suite's corpus, through recall by
/// naming, and count what came back.
pub fn gate(suites: &[Suite], naming: &Naming, now: Timestamp) -> Result<Vec<GateRow>, EvalError> {
    let mut rows = Vec::with_capacity(suites.len());
    for (index, suite) in suites.iter().enumerate() {
        let corpus = Corpus::build(suite, now)?;
        let mut row = GateRow {
            corpus: suite.name.clone(),
            home: 0,
            home_fired: 0,
            home_right: 0,
            cross: 0,
            cross_fired: 0,
        };

        for case in &suite.cases {
            row.home += 1;
            let hits =
                corpus
                    .store
                    .pages_named_by(corpus.project_id, &case.query, OFFERED, naming)?;
            let Some(first) = hits.first() else {
                continue;
            };
            row.home_fired += 1;
            let relevant = case
                .relevant
                .iter()
                .map(|path| normalise(path))
                .collect::<Result<Vec<_>, _>>()?;
            if relevant.iter().any(|path| path == first.path.as_str()) {
                row.home_right += 1;
            }
        }

        for (other, foreign) in suites.iter().enumerate() {
            if other == index {
                continue;
            }
            for case in &foreign.cases {
                row.cross += 1;
                let hits =
                    corpus
                        .store
                        .pages_named_by(corpus.project_id, &case.query, OFFERED, naming)?;
                if !hits.is_empty() {
                    row.cross_fired += 1;
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_suites;

    fn now() -> Timestamp {
        "2026-09-19T09:00:00Z".parse().expect("timestamp")
    }

    fn suites() -> Vec<Suite> {
        builtin_suites()
            .into_iter()
            .map(|(_, source)| Suite::from_toml(source).expect("suite"))
            .collect()
    }

    /// Every corpus is asked every other suite's questions, and its own.
    #[test]
    fn every_question_is_asked_of_every_corpus() {
        let suites = suites();
        let total: usize = suites.iter().map(|suite| suite.cases.len()).sum();
        let rows = gate(&suites, &Naming::default(), now()).expect("gate");

        assert_eq!(rows.len(), suites.len());
        for (row, suite) in rows.iter().zip(&suites) {
            assert_eq!(row.home, suite.cases.len());
            assert_eq!(row.home + row.cross, total);
        }
    }

    /// The bar, kept loose on purpose: these are four small corpora, and a
    /// gate tuned until they are perfect is a gate tuned to them. What it
    /// catches is a change that makes recall chatty — the failure that
    /// teaches an agent to skip the block — or one that makes it mute.
    ///
    /// Measured when this was written: 1 of 171 false alarms, 21 right blocks
    /// of 57. `anamnesis eval --gate` prints the table.
    #[test]
    fn recall_by_name_keeps_quiet_where_it_has_nothing_to_say() {
        let rows = gate(&suites(), &Naming::default(), now()).expect("gate");
        let cross: usize = rows.iter().map(|row| row.cross).sum();
        let false_alarms: usize = rows.iter().map(|row| row.cross_fired).sum();
        let right: usize = rows.iter().map(|row| row.home_right).sum();
        assert!(
            false_alarms <= 3,
            "{false_alarms} of {cross} unanswerable questions got a block"
        );
        assert!(
            right >= 18,
            "only {right} own questions led with their answer"
        );
    }
}
