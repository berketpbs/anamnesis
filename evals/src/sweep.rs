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
//! chosen after the numbers are in: nothing may fall on any suite, and
//! something has to rise.

use std::collections::HashMap;
use std::sync::Mutex;

use anamnesis_core::embedding::Embed;
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
    /// Nothing may fall — not recall, not rank, on any suite — and something
    /// must rise. A setting that buys rank by dropping an answer out of the
    /// results is not an improvement in retrieval, it is a narrower search.
    ///
    /// This originally demanded a rise on *every* suite, which was written
    /// before either suite could reach a perfect score and turned out to
    /// exclude the only interesting case: once `retrieval` sat at 1.000, no
    /// setting could ever raise it, so a setting that took `crowded` from
    /// 0.967 to 1.000 was reported as no improvement at all. Six thousand rows
    /// scored and none qualified, which is a rule failing rather than a grid
    /// failing.
    ///
    /// The condition it cannot express — that a candidate wins as a region of
    /// the grid rather than as a single spike — is still left to whoever reads
    /// the table.
    pub fn improves_on(&self, baseline: &SweepPoint) -> bool {
        if self.scores.len() != baseline.scores.len() {
            return false;
        }
        let pairs = || self.scores.iter().zip(&baseline.scores);

        let nothing_falls = pairs().all(|(mine, theirs)| {
            mine.recall >= theirs.recall - f64::EPSILON && mine.mrr >= theirs.mrr - f64::EPSILON
        });
        let something_rises = pairs().any(|(mine, theirs)| mine.mrr > theirs.mrr + f64::EPSILON);

        nothing_falls && something_rises
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
/// - `entity_coverage` decides how much of a name the query has to say. One
///   requires all of it, which is what the stream was written to do and had
///   never been measured against saying half. Half rather than anything
///   smaller because every multi-token name either suite asks about is two
///   tokens long, where "half" and "any" are the same request.
/// - `candidates` decides how deep each stream reaches before its ranking is
///   fused. It cuts both ways, which is why it is here rather than assumed: a
///   shallow pool cannot answer with a page no stream rated highly, and a deep
///   one lets three streams' also-rans outvote one stream's favourite.
///
/// - `vectors` is swept only because there is finally something to weigh:
///   until the stream was populated on every write path it was empty in every
///   run, and a weight over nothing measures nothing. Without `--embed` it
///   still is, and the three values collapse to one answer.
///
/// Every knob spans values on both sides of the one that ships, and the entity
/// weight is allowed past full-text's. A grid whose best row sits on its own
/// edge has not found an optimum, only a direction, and picking the edge is
/// how a sweep talks somebody into a value it never actually measured.
pub fn default_grid() -> Vec<Tuning> {
    let mut grid = Vec::new();
    for rrf_k in [1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0] {
        for entity in [0.5, 1.0, 1.5] {
            for links in [0.0, 0.25, 0.5, 1.0] {
                for authority_exponent in [0.0, 0.25, 0.5, 1.0] {
                    for candidates in [10, 30, 120] {
                        for entity_coverage in [0.5, 1.0] {
                            for vectors in [0.0, 0.5, 1.0] {
                                grid.push(Tuning {
                                    rrf_k,
                                    fts: 1.0,
                                    entity,
                                    links,
                                    vectors,
                                    authority_exponent,
                                    entity_coverage,
                                    candidates,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    grid
}

/// An embedder that answers the same question once.
///
/// A sweep asks every suite's questions once per setting — two thousand
/// settings against twenty-five questions is fifty thousand calls to a model,
/// for twenty-five distinct answers. Every one of them is deterministic, so
/// the second is the first.
///
/// Only the queries pass through here. The corpus is embedded once, when it is
/// built, and reused across the whole grid for the same reason.
struct Memo<'a> {
    inner: &'a dyn Embed,
    seen: Mutex<HashMap<String, Vec<f32>>>,
}

impl<'a> Memo<'a> {
    fn new(inner: &'a dyn Embed) -> Self {
        Self {
            inner,
            seen: Mutex::new(HashMap::new()),
        }
    }
}

impl Embed for Memo<'_> {
    fn model(&self) -> &str {
        self.inner.model()
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        if let Some(cached) = self.seen.lock().expect("not poisoned").get(text) {
            return Ok(cached.clone());
        }
        let vector = self.inner.embed(text)?;
        self.seen
            .lock()
            .expect("not poisoned")
            .insert(text.to_owned(), vector.clone());
        Ok(vector)
    }
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
    embedder: Option<&dyn anamnesis_core::embedding::Embed>,
) -> Result<SweepReport, EvalError> {
    let memo = embedder.map(Memo::new);
    let embedder = memo.as_ref().map(|memo| memo as &dyn Embed);

    let mut corpora = Vec::with_capacity(suites.len());
    for (_, suite) in suites {
        corpora.push(Corpus::build_with(suite, now, embedder)?);
    }

    let mut points = Vec::with_capacity(grid.len());
    for tuning in grid {
        let mut scores = Vec::with_capacity(suites.len());
        for ((name, suite), corpus) in suites.iter().zip(&corpora) {
            let report = run_on(corpus, suite, now, tuning, embedder)?;
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
        let report = sweep(&suites(), now(), &grid, None).expect("sweep");

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
        let report = sweep(&suites(), now(), &grid, None).expect("sweep");

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

        // Better on one corpus and unchanged on the other is an improvement,
        // and the rule used to say otherwise. It was written before either
        // suite could reach a perfect score; once `retrieval` sat at 1.000 no
        // setting could raise it, so nothing that fixed the *other* corpus
        // could ever qualify. Six thousand rows, none accepted.
        let one_better_one_level = candidate(vec![
            SuiteScore {
                suite: "a".into(),
                mrr: 0.50,
                recall: 1.00,
            },
            SuiteScore {
                suite: "b".into(),
                mrr: 0.55,
                recall: 0.80,
            },
        ]);
        assert!(
            one_better_one_level.improves_on(&baseline),
            "a corpus at its ceiling must not veto a gain on the other"
        );

        let one_better_one_worse = candidate(vec![
            SuiteScore {
                suite: "a".into(),
                mrr: 0.90,
                recall: 1.00,
            },
            SuiteScore {
                suite: "b".into(),
                mrr: 0.30,
                recall: 0.80,
            },
        ]);
        assert!(
            !one_better_one_worse.improves_on(&baseline),
            "a gain paid for on the other corpus is the fitted result to refuse"
        );

        assert!(
            !baseline.improves_on(&baseline),
            "unchanged everywhere is not an improvement"
        );
    }
}
