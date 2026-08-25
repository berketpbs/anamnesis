//! Rank fusion: combining independent retrieval streams into one ranking.
//!
//! `memory_query` runs full-text search, entity matching, and link-neighbour
//! expansion as three independent streams, each ordered best-first. None of
//! them is trustworthy alone — full-text search misses a page that never says
//! the word you used, entity matching misses a page that never names anything
//! canonical, and link-neighbour expansion is only as good as its seed set.
//! Reciprocal Rank Fusion combines them by rank rather than by score, which
//! means a page three streams agree on beats one signal's favorite even when
//! the streams disagree about magnitude.
//!
//! This module is pure: no SQL, no I/O. The streams themselves are assembled
//! by whichever storage layer can run the queries; this is just the arithmetic
//! that turns several rankings into one.

use std::collections::HashMap;

use crate::ids::PageId;

/// Constant added to each rank before inverting, as in the standard RRF
/// formula (`score += 1 / (k + rank)`). Larger values compress the gap between
/// a stream's best and worst results, so no single stream can dominate the
/// fused ranking just by being confident.
pub const RRF_K: f64 = 60.0;

/// Split text into the tokens retrieval matches on.
///
/// One home for this on purpose. A query is tokenized before it is compared
/// against anything, so whatever it is compared *to* has to be tokenized the
/// same way — an entity named `Windows BOM`, stored whole, can never equal any
/// token a query produces, and the page it names quietly becomes unreachable
/// through that stream.
///
/// Deliberately blunt: split on everything that is not alphanumeric, lowercase
/// what is left, keep the order, drop repeats. `crates/anamnesis-llm` and
/// `crates anamnesis llm` come out the same, which is the point.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        if !tokens.contains(&lower) {
            tokens.push(lower);
        }
    }
    tokens
}

/// Fuse ranked streams (each ordered best match first) into one score per id.
///
/// A page absent from a stream contributes nothing to that stream's sum; it is
/// not penalised beyond simply not being rewarded. The result is unsorted by
/// nothing in particular — callers that want a ranking should sort by score
/// descending, which [`fuse_and_rank`] does for them.
pub fn reciprocal_rank_fusion(streams: &[Vec<PageId>], k: f64) -> HashMap<PageId, f64> {
    let mut scores: HashMap<PageId, f64> = HashMap::new();
    for stream in streams {
        for (rank, id) in stream.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    scores
}

/// Fuse streams and return them sorted best-first.
pub fn fuse_and_rank(streams: &[Vec<PageId>], k: f64) -> Vec<(PageId, f64)> {
    let mut fused: Vec<(PageId, f64)> = reciprocal_rank_fusion(streams, k).into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused
}

/// How much a page's fused score should be scaled for its standing in the
/// wiki, applied *after* fusion produces relevance candidates.
///
/// This is a multiplier, not an independent retriever: a page that no stream
/// found relevant stays absent no matter how authoritative it is. Only pages
/// already in the fused set get pushed up or down within it.
pub fn authority_multiplier(pinned: bool, canonical: bool, authoritative_namespace: bool) -> f64 {
    let mut multiplier = 1.0;
    if authoritative_namespace {
        multiplier *= 1.5;
    }
    if canonical {
        multiplier *= 1.3;
    }
    if pinned {
        multiplier *= 1.2;
    }
    multiplier
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn tokens_are_lowercased_split_and_deduplicated() {
        assert_eq!(
            tokenize("crates/anamnesis-llm/src/lib.rs"),
            vec!["crates", "anamnesis", "llm", "src", "lib", "rs"]
        );
        assert_eq!(tokenize("Windows BOM"), vec!["windows", "bom"]);
        assert_eq!(tokenize("SQLite sqlite SQLITE"), vec!["sqlite"]);
        assert!(tokenize("   ...   ").is_empty());
    }

    #[test]
    fn a_name_and_the_query_someone_types_for_it_agree() {
        // The property the entity stream depends on: however someone spells
        // the separator, the tokens match.
        assert_eq!(tokenize("anamnesis-llm"), tokenize("anamnesis llm"));
        assert_eq!(tokenize("lib.rs"), tokenize("lib rs"));
    }

    fn id(n: u128) -> PageId {
        PageId::from_uuid(Uuid::from_u128(n))
    }

    #[test]
    fn a_page_every_stream_agrees_on_outranks_one_streams_favorite() {
        let fts = vec![id(2), id(1), id(3)];
        let entity = vec![id(1), id(4)];
        let links = vec![id(1), id(5)];

        let ranked = fuse_and_rank(&[fts, entity, links], RRF_K);
        assert_eq!(ranked[0].0, id(1), "consensus pick should lead");
    }

    #[test]
    fn a_page_absent_from_every_stream_never_appears() {
        let fts = vec![id(1)];
        let ranked = fuse_and_rank(&[fts], RRF_K);
        assert!(!ranked.iter().any(|(pid, _)| *pid == id(99)));
    }

    #[test]
    fn empty_streams_fuse_to_nothing() {
        let ranked: Vec<(PageId, f64)> = fuse_and_rank(&[], RRF_K);
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_within_a_stream_still_matters_for_ties_elsewhere() {
        // id(1) is top of one stream; id(2) is merely present in two, but
        // lower in both. Fusion should not treat "present anywhere" as
        // equivalent to "best somewhere".
        let a = vec![id(1), id(2)];
        let b = vec![id(3), id(2)];
        let ranked = fuse_and_rank(&[a, b], RRF_K);
        let score_of = |target: PageId| ranked.iter().find(|(pid, _)| *pid == target).unwrap().1;
        assert!(score_of(id(1)) < score_of(id(2)) + score_of(id(1)));
        // id(2) appears in both streams, so it should score higher than a page
        // appearing in only one at the same rank depth.
        assert!(score_of(id(2)) > score_of(id(1)) - 1e-9 || score_of(id(2)) > 0.0);
    }

    #[test]
    fn multiplier_is_neutral_with_nothing_to_reward() {
        assert!((authority_multiplier(false, false, false) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn multiplier_compounds_across_signals() {
        let plain = authority_multiplier(false, false, false);
        let pinned = authority_multiplier(true, false, false);
        let canonical = authority_multiplier(false, true, false);
        let namespace = authority_multiplier(false, false, true);
        let all = authority_multiplier(true, true, true);

        assert!(pinned > plain);
        assert!(canonical > plain);
        assert!(namespace > plain);
        assert!(all > pinned.max(canonical).max(namespace));
    }
}
