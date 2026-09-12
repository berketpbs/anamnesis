//! What each retrieval stream contributes, and what would be lost without it.
//!
//! The fused score in [`crate::run()`] says how good retrieval is. It cannot say
//! *why*, and the questions worth asking are all why-questions: does the
//! entity stream earn its place, do link neighbours help or merely add noise,
//! what does the opt-in embedder buy. In a fused ranking a page three streams
//! agreed on and a page one stream insisted on look identical.
//!
//! So each stream is scored on its own, and then on the one measure that
//! actually decides whether a stream stays: how many questions **only** it can
//! answer. A stream with a respectable average and nothing unique to
//! contribute is a stream the others already cover.

use anamnesis_core::ids::PageId;
use anamnesis_core::retrieval::Tuning;
use jiff::Timestamp;

use crate::EvalError;
use crate::corpus::Corpus;
use crate::score::{CaseScore, mean_reciprocal_rank, recall, score_case};
use crate::suite::Suite;

/// How one stream did across a whole suite.
#[derive(Debug, Clone)]
pub struct StreamScore {
    /// Which stream: `fts`, `entity`, `links`, or `vectors`.
    pub name: &'static str,
    /// Mean reciprocal rank of this stream alone.
    pub mrr: f64,
    /// Share of cases this stream could answer at all.
    pub recall: f64,
    /// Cases no other stream found — what deleting this stream would cost.
    pub only_stream_to_find: Vec<String>,
}

/// Every stream's contribution to one suite.
///
/// The three lists partition the questions by two facts — did any stream
/// answer on its own, and did the fused ranking answer — and leave out only the
/// ordinary case where both did. They are kept apart because they are three
/// different findings. There used to be one list, of questions no stream
/// answered, printed under "fusion is doing the work"; nothing checked whether
/// fusion had answered them either, and on the first suite with questions
/// nothing answers, every one of them was reported as fusion's success.
#[derive(Debug, Clone)]
pub struct Ablation {
    /// One entry per stream, in the order they are fused.
    pub streams: Vec<StreamScore>,
    /// Questions no single stream answered, which the fused ranking did.
    ///
    /// Where fusion is doing the work rather than any one signal: each stream
    /// is scored over the suite's window, while fusion draws on every
    /// stream's deeper candidates, so a page several streams half agreed on
    /// can come back when no stream put it in its own first few.
    pub fusion_only: Vec<String>,
    /// Questions some stream answered, which the fused ranking did not.
    ///
    /// The most actionable of the three. The answer was in hand and the
    /// weighing of streams threw it away — the shape of the fault the
    /// 2026-08-29 sweep was run to fix.
    pub lost_in_fusion: Vec<String>,
    /// Questions nothing answered, alone or fused.
    pub missed: Vec<String>,
}

/// Score each stream separately over a suite.
pub fn ablate(suite: &Suite, now: Timestamp) -> Result<Ablation, EvalError> {
    ablate_with(suite, now, None)
}

/// The same, with the embedding stream switched on.
pub fn ablate_with(
    suite: &Suite,
    now: Timestamp,
    embedder: Option<&dyn anamnesis_core::embedding::Embed>,
) -> Result<Ablation, EvalError> {
    let corpus = Corpus::build_with(suite, now, embedder)?;

    // Indexed by stream, then by case: `per_stream[s][c]` is how stream `s`
    // did on case `c`.
    let mut names: Vec<&'static str> = Vec::new();
    let mut per_stream: Vec<Vec<CaseScore>> = Vec::new();
    let mut fusion_only = Vec::new();
    let mut lost_in_fusion = Vec::new();
    let mut missed = Vec::new();

    for (index, case) in suite.cases.iter().enumerate() {
        let vector = embedder.and_then(|embedder| {
            embedder
                .embed(&case.query)
                .ok()
                .map(|vector| (embedder.model().to_owned(), vector))
        });
        let embedding = vector
            .as_ref()
            .map(|(model, vector)| (model.as_str(), vector.as_slice()));
        let breakdown = corpus.store.query_streams(
            corpus.project_id,
            &case.query,
            suite.limit,
            embedding,
            &Tuning::default(),
        )?;

        // Asked through the same call a run makes, so "fusion answered it"
        // here means what it means in the report printed above this one.
        let fused: Vec<String> = corpus
            .store
            .query_pages_with(
                corpus.project_id,
                &case.query,
                suite.limit,
                now,
                embedding,
                &Tuning::default(),
            )?
            .iter()
            .map(|hit| hit.path.as_str().to_owned())
            .collect();
        let fused_found = score_case(&fused, &case.relevant).found();

        let mut any_found = false;
        for (position, (name, ranking)) in breakdown.named().iter().enumerate() {
            if index == 0 {
                names.push(name);
                per_stream.push(Vec::with_capacity(suite.cases.len()));
            }
            let score = score_case(&paths(&corpus, ranking)?, &case.relevant);
            any_found |= score.found();
            per_stream[position].push(score);
        }

        match (any_found, fused_found) {
            (true, true) => {}
            (false, true) => fusion_only.push(case.query.clone()),
            (true, false) => lost_in_fusion.push(case.query.clone()),
            (false, false) => missed.push(case.query.clone()),
        }
    }

    let streams = names
        .into_iter()
        .enumerate()
        .map(|(position, name)| StreamScore {
            name,
            mrr: mean_reciprocal_rank(&per_stream[position]),
            recall: recall(&per_stream[position]),
            only_stream_to_find: unique_to(&per_stream, position, suite),
        })
        .collect();

    Ok(Ablation {
        streams,
        fusion_only,
        lost_in_fusion,
        missed,
    })
}

/// The cases stream `position` found and every other stream missed.
fn unique_to(per_stream: &[Vec<CaseScore>], position: usize, suite: &Suite) -> Vec<String> {
    (0..suite.cases.len())
        .filter(|case| {
            per_stream[position][*case].found()
                && per_stream
                    .iter()
                    .enumerate()
                    .all(|(other, scores)| other == position || !scores[*case].found())
        })
        .map(|case| suite.cases[case].query.clone())
        .collect()
}

/// Turn a stream's page ids into the paths a case is written in terms of.
///
/// A suite names pages the way a person does; a stream returns identifiers.
/// One translation, here, rather than the cases having to know about ids.
fn paths(corpus: &Corpus, ranking: &[PageId]) -> Result<Vec<String>, EvalError> {
    let known = corpus.store.page_paths(corpus.project_id)?;
    Ok(ranking
        .iter()
        .filter_map(|id| {
            known
                .iter()
                .find(|(page_id, _)| page_id == id)
                .map(|(_, path)| path.clone())
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-08-28T09:00:00Z".parse().expect("timestamp")
    }

    const SUITE: &str = r#"
name = "ablation-test"
description = "one question only the entity stream can answer"

[[page]]
path = "decisions/0001-storage.md"
title = "Storage engine"
tier = "semantic"
entities = ["SQLite"]
body = "The index is one file on disk, which is the whole reason."

[[page]]
path = "notes/windows.md"
title = "Windows notes"
body = "PowerShell prepends a byte order mark when piping."

[[case]]
query = "sqlite"
relevant = ["decisions/0001-storage.md"]

[[case]]
query = "byte order mark"
relevant = ["notes/windows.md"]
"#;

    /// The measurement the whole module exists for: a page whose body never
    /// says the word is reachable only because it declared an entity, and the
    /// report has to name that as the entity stream's own contribution.
    #[test]
    fn a_stream_gets_credit_for_what_only_it_finds() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let ablation = ablate(&suite, now()).expect("ablate");

        let entity = ablation
            .streams
            .iter()
            .find(|stream| stream.name == "entity")
            .expect("the entity stream is scored");
        assert_eq!(
            entity.only_stream_to_find,
            vec!["sqlite".to_owned()],
            "the entity stream is the only one that can reach a page by a name its body never says"
        );

        let fts = ablation
            .streams
            .iter()
            .find(|stream| stream.name == "fts")
            .expect("the full-text stream is scored");
        assert!(fts.recall > 0.0, "full text still answers its own question");
    }

    /// Every stream is reported, including the ones that found nothing, or
    /// "which streams are pulling their weight" cannot be read off the table.
    #[test]
    fn every_stream_is_reported_even_when_it_found_nothing() {
        let suite = Suite::from_toml(SUITE).expect("suite");
        let ablation = ablate(&suite, now()).expect("ablate");

        assert_eq!(ablation.streams.len(), 4);
        let vectors = ablation
            .streams
            .iter()
            .find(|stream| stream.name == "vectors")
            .expect("the vector stream is scored");
        // No embedder here, which is the ordinary case: it is opt-in.
        assert_eq!(vectors.recall, 0.0);
        assert!(vectors.only_stream_to_find.is_empty());
    }

    /// The fault the three lists replaced. A question nothing answers used to
    /// be printed under "fusion is doing the work", because the one list it
    /// went into never asked whether fusion had answered it either.
    #[test]
    fn a_question_nothing_answers_is_a_miss_rather_than_fusion_at_work() {
        let source = SUITE.replace("query = \"sqlite\"", "query = \"kubernetes ingress\"");
        let suite = Suite::from_toml(&source).expect("suite");
        let ablation = ablate(&suite, now()).expect("ablate");

        assert_eq!(ablation.missed, vec!["kubernetes ingress".to_owned()]);
        assert!(ablation.fusion_only.is_empty(), "{ablation:?}");
        assert!(ablation.lost_in_fusion.is_empty(), "{ablation:?}");
    }

    /// What the old list was meant to hold, and still has to. Each stream is
    /// scored over the suite's window of one; fusion reaches deeper. Full text
    /// puts the answer second behind a page that says `alpha` three times, the
    /// entity stream puts it second behind a page with a rarer name, and the
    /// two second places together outscore either first place.
    #[test]
    fn a_question_only_fusion_answers_is_credited_to_fusion() {
        let source = r#"
name = "fusion-only"
description = "two streams each rank the answer second"
limit = 1

[[page]]
path = "notes/loud.md"
title = "Loud"
body = "alpha alpha alpha"

[[page]]
path = "notes/named.md"
title = "Named"
entities = ["alpha", "beta"]
body = "Nothing on this page matches the question in its text."

[[page]]
path = "notes/answer.md"
title = "Answer"
entities = ["beta"]
body = "A longer page that says alpha once, somewhere in the middle of a good many other words about other things."

[[case]]
query = "alpha beta"
relevant = ["notes/answer.md"]
"#;
        let suite = Suite::from_toml(source).expect("suite");
        let ablation = ablate(&suite, now()).expect("ablate");

        assert_eq!(
            ablation.fusion_only,
            vec!["alpha beta".to_owned()],
            "{ablation:?}"
        );
        assert!(ablation.missed.is_empty(), "{ablation:?}");
        assert!(ablation.lost_in_fusion.is_empty(), "{ablation:?}");
    }

    /// The finding worth the most, and the one no list reported at all. The
    /// entity stream has the answer first; full text has a pinned, canonical
    /// decision first; the authority multiplier breaks the tie the wrong way,
    /// and over a window of one the answer is gone.
    #[test]
    fn a_question_a_stream_answered_and_fusion_dropped_is_named() {
        let source = r#"
name = "lost-in-fusion"
description = "authority outweighs the stream that had the answer"
limit = 1

[[page]]
path = "notes/storage.md"
title = "Storage"
entities = ["SQLite"]
body = "One file on disk, and no server to run."

[[page]]
path = "decisions/0002-database.md"
title = "Database"
tier = "semantic"
canonical = true
pinned = true
body = "We use sqlite."

[[case]]
query = "sqlite"
relevant = ["notes/storage.md"]
"#;
        let suite = Suite::from_toml(source).expect("suite");
        let ablation = ablate(&suite, now()).expect("ablate");

        assert_eq!(
            ablation.lost_in_fusion,
            vec!["sqlite".to_owned()],
            "{ablation:?}"
        );
        assert!(ablation.fusion_only.is_empty(), "{ablation:?}");
        assert!(ablation.missed.is_empty(), "{ablation:?}");
    }

    /// The suite that ships is the one this is worth running against.
    #[test]
    fn the_builtin_suite_can_be_ablated() {
        let suite = Suite::from_toml(crate::RETRIEVAL_SUITE).expect("suite");
        let ablation = ablate(&suite, now()).expect("ablate");
        assert_eq!(ablation.streams.len(), 4);
    }
}
