//! Scoring the same questions once per setting.
//!
//! Every number in [`anamnesis_core::retrieval`] was chosen by argument. A
//! sweep replaces the argument with a table: the corpus is built once, and
//! then asked the same questions under each candidate setting, through the
//! same call the server makes.
//!
//! What a sweep cannot do is decide. Ten or fifteen questions are far too few
//! to crown a winner, and the setting that tops the table will always be the
//! one that suits those questions best — which is why the rule for accepting
//! one ([`SweepPoint::improves_on`]) is written here, in code, rather than
//! chosen after the numbers are in: a candidate has to hold recall on **every**
//! suite and raise the mean rank on **every** suite, not on average across
//! them.

use anamnesis_core::retrieval::Tuning;
use jiff::Timestamp;

use crate::EvalError;
use crate::corpus::Corpus;
use crate::run::run_on;
use crate::suite::Suite;

/// How one suite scored under one setting.
#[derive(Debug, Clone)]
pub struct SuiteScore {
    /// Which suite.
    pub suite: String,
    /// Mean reciprocal rank.
    pub mrr: f64,
    /// Share of questions answered at all.
    pub recall: f64,
}

/// One setting, and what it scored everywhere.
#[derive(Debug, Clone)]
pub struct SweepPoint {
    /// The setting these scores belong to.
    pub tuning: Tuning,
    /// One entry per suite, in the order the suites were given.
    pub scores: Vec<SuiteScore>,
}

impl SweepPoint {
    /// Mean rank across the suites — the one number a table can be sorted by.
    ///
    /// Only a sort key. Deciding by it would let a large gain on one corpus
    /// pay for a loss on the other, which is the thing a second corpus exists
    /// to prevent.
    pub fn mean_mrr(&self) -> f64 {
        if self.scores.is_empty() {
            return 0.0;
        }
        self.scores.iter().map(|score| score.mrr).sum::<f64>() / self.scores.len() as f64
    }

    /// Whether this setting is the one that ships.
    pub fn is_default(&self) -> bool {
        self.tuning == Tuning::default()
    }

    /// Whether this setting is one the project would accept over `baseline`.
    ///
    /// Both conditions on every suite: recall never falls, and rank rises. A
    /// setting that buys rank by dropping an answer out of the results is not
    /// an improvement in retrieval, it is a narrower search.
    ///
    /// The third condition — that a candidate wins as a region of the grid
    /// rather than as a single spike — cannot be read off one point, and is
    /// left to whoever reads the table.
    pub fn improves_on(&self, baseline: &SweepPoint) -> bool {
        if self.scores.len() != baseline.scores.len() {
            return false;
        }
        self.scores
            .iter()
            .zip(&baseline.scores)
            .all(|(mine, theirs)| {
                mine.recall >= theirs.recall - f64::EPSILON && mine.mrr > theirs.mrr + f64::EPSILON
            })
    }
}

/// Every setting tried, and the suites they were tried on.
#[derive(Debug, Clone)]
pub struct SweepReport {
    /// The suites, in the order their scores appear in each point.
    pub suites: Vec<String>,
    /// One point per setting, best mean rank first.
    pub points: Vec<SweepPoint>,
}

impl SweepReport {
    /// The setting that ships today, if it was among those tried.
    ///
    /// A sweep whose grid does not contain the current defaults measures
    /// nothing anybody can act on: every row would be a change, with no row
    /// saying what changing costs.
    pub fn baseline(&self) -> Option<&SweepPoint> {
        self.points.iter().find(|point| point.is_default())
    }

    /// The settings that clear [`SweepPoint::improves_on`] against today's.
    pub fn improvements(&self) -> Vec<&SweepPoint> {
        let Some(baseline) = self.baseline() else {
            return Vec::new();
        };
        self.points
            .iter()
            .filter(|point| point.improves_on(baseline))
            .collect()
    }
}

/// The settings a sweep tries when it is not told otherwise.
///
/// Three knobs and the constant, chosen because each has an argument behind it
/// that arithmetic already casts doubt on:
///
/// - `rrf_k` decides how much rank within a stream matters. At 60, the whole
///   thirty-deep spread of a stream is 1.47x, so being present in two streams
///   at any depth beats being first in one.
/// - The entity and link weights decide how much a second and third opinion
///   count. Full-text weight is deliberately **not** swept: fusion scores are
///   relative, so scaling every other stream against a fixed `fts = 1.0`
///   already covers "trust full text more", and sweeping it too would only
///   produce duplicate rows.
/// - `authority_exponent` decides how much a canonical page in an
///   authoritative namespace is allowed to outrank a relevant one. At 1.0 the
///   multiplier reaches 2.34x, which is larger than the entire spread it
///   adjusts.
///
/// The vector weight is left alone: the stream is opt-in and empty here, so
/// sweeping it would measure nothing.
///
/// Every knob spans values on both sides of the one that ships, and the entity
/// weight is allowed past full-text's. A grid whose best row sits on its own
/// edge has not found an optimum, only a direction, and picking the edge is
/// how a sweep talks somebody into a value it never actually measured.
pub fn default_grid() -> Vec<Tuning> {
    let mut grid = Vec::new();
    for rrf_k in [2.0, 5.0, 10.0, 20.0, 30.0, 60.0] {
        for entity in [0.5, 1.0, 1.5] {
            for links in [0.0, 0.25, 0.5, 1.0] {
                for authority_exponent in [0.0, 0.25, 0.5, 1.0] {
                    grid.push(Tuning {
                        rrf_k,
                        fts: 1.0,
                        entity,
                        links,
                        vectors: 1.0,
                        authority_exponent,
                    });
                }
            }
        }
    }
    grid
}

/// Score every setting in `grid` against every suite.
///
/// Each corpus is built once and reused across the whole grid. That is the
/// only reason a sweep of this size finishes: a corpus is pages written into a
/// git repository, one commit each, while a query is a handful of statements
/// against a small database.
pub fn sweep(
    suites: &[(String, Suite)],
    now: Timestamp,
    grid: &[Tuning],
) -> Result<SweepReport, EvalError> {
    let mut corpora = Vec::with_capacity(suites.len());
    for (_, suite) in suites {
        corpora.push(Corpus::build(suite, now)?);
    }

    let mut points = Vec::with_capacity(grid.len());
    for tuning in grid {
        let mut scores = Vec::with_capacity(suites.len());
        for ((name, suite), corpus) in suites.iter().zip(&corpora) {
            let report = run_on(corpus, suite, now, tuning)?;
            scores.push(SuiteScore {
                suite: name.clone(),
                mrr: report.mrr,
                recall: report.recall,
            });
        }
        points.push(SweepPoint {
            tuning: *tuning,
            scores,
        });
    }

    points.sort_by(|a, b| {
        b.mean_mrr()
            .partial_cmp(&a.mean_mrr())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(SweepReport {
        suites: suites.iter().map(|(name, _)| name.clone()).collect(),
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-08-28T09:00:00Z".parse().expect("timestamp")
    }

    const SUITE: &str = r#"
name = "sweep-test"
description = "small enough to sweep in a test"

[[page]]
path = "decisions/0001-storage.md"
title = "Storage"
tier = "semantic"
canonical = true
entities = ["SQLite"]
body = "One file on disk. See [[notes/windows.md]]."

[[page]]
path = "notes/windows.md"
title = "Windows notes"
body = "PowerShell prepends a byte order mark when piping."

[[case]]
query = "byte order mark"
relevant = ["notes/windows.md"]

[[case]]
query = "sqlite"
relevant = ["decisions/0001-storage.md"]
"#;

    fn suites() -> Vec<(String, Suite)> {
        vec![(
            "sweep-test".to_owned(),
            Suite::from_toml(SUITE).expect("suite"),
        )]
    }

    /// The grid has to contain the setting that ships, or the table says what
    /// every alternative scores and nothing about what they are alternatives
    /// to.
    #[test]
    fn the_grid_contains_todays_defaults() {
        assert!(
            default_grid()
                .iter()
                .any(|tuning| *tuning == Tuning::default()),
            "the current defaults are not among the settings tried"
        );
    }

    #[test]
    fn a_sweep_scores_every_setting_on_every_suite() {
        let grid = vec![
            Tuning::default(),
            Tuning {
                rrf_k: 10.0,
                ..Tuning::default()
            },
        ];
        let report = sweep(&suites(), now(), &grid).expect("sweep");

        assert_eq!(report.points.len(), 2);
        assert_eq!(report.suites, vec!["sweep-test".to_owned()]);
        for point in &report.points {
            assert_eq!(point.scores.len(), 1);
        }
        assert!(report.baseline().is_some(), "the default has to be found");
    }

    /// The same setting, scored twice, has to agree — a sweep reuses one
    /// corpus across the whole grid, and querying it records accesses, so a
    /// score that drifted with query order would make every row suspect.
    #[test]
    fn the_same_setting_scores_the_same_wherever_it_sits_in_the_grid() {
        let grid = vec![Tuning::default(), Tuning::default()];
        let report = sweep(&suites(), now(), &grid).expect("sweep");

        assert_eq!(
            report.points[0].scores[0].mrr,
            report.points[1].scores[0].mrr
        );
        assert_eq!(
            report.points[0].scores[0].recall,
            report.points[1].scores[0].recall
        );
    }

    /// The acceptance rule, stated as a test so it cannot quietly become
    /// "whichever row is highest".
    #[test]
    fn a_setting_that_buys_rank_with_recall_is_not_an_improvement() {
        let baseline = SweepPoint {
            tuning: Tuning::default(),
            scores: vec![
                SuiteScore {
                    suite: "a".into(),
                    mrr: 0.50,
                    recall: 1.00,
                },
                SuiteScore {
                    suite: "b".into(),
                    mrr: 0.40,
                    recall: 0.80,
                },
            ],
        };

        let candidate = |scores: Vec<SuiteScore>| SweepPoint {
            tuning: Tuning {
                rrf_k: 10.0,
                ..Tuning::default()
            },
            scores,
        };

        let better = candidate(vec![
            SuiteScore {
                suite: "a".into(),
                mrr: 0.60,
                recall: 1.00,
            },
            SuiteScore {
                suite: "b".into(),
                mrr: 0.50,
                recall: 0.85,
            },
        ]);
        assert!(better.improves_on(&baseline));

        let lost_recall = candidate(vec![
            SuiteScore {
                suite: "a".into(),
                mrr: 0.90,
                recall: 0.70,
            },
            SuiteScore {
                suite: "b".into(),
                mrr: 0.90,
                recall: 0.90,
            },
        ]);
        assert!(
            !lost_recall.improves_on(&baseline),
            "a higher rank on a smaller set of answers is not better retrieval"
        );

        let only_one_suite = candidate(vec![
            SuiteScore {
                suite: "a".into(),
                mrr: 0.90,
                recall: 1.00,
            },
            SuiteScore {
                suite: "b".into(),
                mrr: 0.40,
                recall: 0.80,
            },
        ]);
        assert!(
            !only_one_suite.improves_on(&baseline),
            "unchanged on the second corpus is exactly the fitted result to refuse"
        );
    }
}
