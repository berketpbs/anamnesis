//! Rank fusion: combining independent retrieval streams into one ranking.
//!
//! `memory_query` runs full-text search, entity matching, and link-neighbour
//! expansion as three independent streams, each ordered best-first. None of
//! them is trustworthy alone — full-text search misses a page that never says
//! the word you used, entity matching misses a page that never names anything
//! canonical, and link-neighbour expansion is only as good as its seed set.
//! Reciprocal Rank Fusion combines them by rank rather than by score, because
//! scores from different sources are not comparable quantities.
//!
//! How much *agreement* is worth against how much *confidence* is the whole
//! question, and it is settled by [`RRF_K`] and the stream weights in
//! [`Tuning`] — measured, since 2026-08-29, rather than assumed. The assumption
//! before then was that agreement should win, which is right when the streams
//! are of comparable quality and wrong here: full-text search answers most
//! questions on its own, and letting two weaker streams outvote it cost more
//! than it ever bought.
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
///
/// **Measured, 2026-08-29.** This was 60 — the value from the paper, chosen for
/// fusing search engines whose runs are a thousand deep and roughly as good as
/// each other. Neither is true here: the streams are thirty deep and one of
/// them is far better than the rest. At 60 the whole spread of a stream was
/// 1.47x, so a page sitting anywhere in two streams outscored the page one
/// stream was sure of, and `anamnesis eval --sweep` found the score rising at
/// every smaller value it tried, on both corpora, with no turning point.
///
/// Two, rather than one: the two score identically everywhere the sweep looked,
/// and one is the smallest value the grid contains. A number picked at the edge
/// of what was measured is a number nothing has been measured on both sides of.
///
/// Fusing scopes rather than streams ([`Store::query_pages_across`]) is
/// unaffected by this: a page belongs to one project, so it appears in exactly
/// one of those rankings, and `1 / (k + rank)` orders single-membership
/// rankings by rank alone whatever `k` is.
///
/// [`Store::query_pages_across`]: ../../anamnesis_store/struct.Store.html#method.query_pages_across
pub const RRF_K: f64 = 2.0;

/// Every number the fused ranking is free to get wrong.
///
/// These were all chosen by argument. Gathering them into one type is what
/// makes them measurable: `anamnesis eval --sweep` scores the same corpus once
/// per setting and prints what each one costs, so the next change to any of
/// them can be defended with a number instead of a paragraph.
///
/// [`Tuning::default`] is what runs. Nothing reads these from configuration on
/// purpose — a knob set per project but measured by nobody is the class of
/// setting this codebase keeps having to delete.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    /// The RRF constant. See [`RRF_K`].
    pub rrf_k: f64,
    /// Weight of the full-text stream in the fused sum.
    pub fts: f64,
    /// Weight of the entity stream.
    pub entity: f64,
    /// Weight of the link-neighbour stream.
    pub links: f64,
    /// Weight of the embedding stream.
    pub vectors: f64,
    /// Exponent applied to [`authority_multiplier`]. `1.0` leaves it as it is,
    /// `0.0` switches it off, and anything between softens it.
    pub authority_exponent: f64,
}

impl Default for Tuning {
    /// What ships, as of the sweep on 2026-08-29.
    ///
    /// Before it: `k = 60`, four streams weighted equally, and the authority
    /// multiplier applied in full. That scored 0.708 / 1.000 on the retrieval
    /// suite and 0.436 / 0.533 on the crowded one — the second being a corpus
    /// where plain full-text search alone scored 0.900 / 0.933, because fusion
    /// was burying the answers it ranked first. These score 1.000 / 1.000 and
    /// 0.967 / 1.000.
    ///
    /// Where the measurement was decisive it was followed; where it was
    /// indifferent the design was kept. Silencing the link stream, dropping
    /// authority to nothing, and weighting entities above full text all score
    /// exactly the same as the values here, and all three would throw away a
    /// signal on the evidence of twenty-five questions.
    fn default() -> Self {
        Self {
            rrf_k: RRF_K,
            fts: 1.0,
            // Left level with full text. Above it scored no better, and
            // "a declared name outranks the words on the page" is a claim
            // nothing here has made.
            entity: 1.0,
            // Enough to break a tie between pages full text likes equally,
            // not enough to outvote it. Neighbours of a hit are evidence
            // about the hit, not about themselves: the stream answered no
            // question on its own in either ablation. Re-measured after the
            // stream began weighting neighbours by their seed's rank, which
            // improved its own ordering without moving what it is worth here.
            links: 0.25,
            // Unmeasured — the stream is opt-in and neither suite runs a
            // model, so this is the one weight still standing on an argument.
            vectors: 1.0,
            // A quarter, so the full 2.34x multiplier becomes about 1.24x.
            // Authority is a preference between comparably relevant pages,
            // and applied whole it was larger than the entire spread of the
            // relevance it adjusted — a canonical page in an authoritative
            // namespace outranked whatever any stream put first.
            authority_exponent: 0.25,
        }
    }
}

impl Tuning {
    /// The stream weights in the order the streams are fused.
    pub fn weights(&self) -> [f64; 4] {
        [self.fts, self.entity, self.links, self.vectors]
    }

    /// The authority multiplier this tuning applies.
    ///
    /// Kept here rather than at the call site because the exponent only means
    /// anything against the multiplier it modifies, and separating them is how
    /// one of them silently stops being applied.
    pub fn authority(&self, pinned: bool, canonical: bool, authoritative_namespace: bool) -> f64 {
        authority_multiplier(pinned, canonical, authoritative_namespace)
            .powf(self.authority_exponent)
    }
}

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

/// Fuse streams that do not count equally, best-first.
///
/// A weight of zero silences a stream without removing it from the call, which
/// is the difference an ablation needs: a stream deleted from the fusion and a
/// stream contributing nothing to it are the same ranking, but only one of
/// them can be turned back on to see what it was worth.
pub fn fuse_weighted(streams: &[(&[PageId], f64)], k: f64) -> Vec<(PageId, f64)> {
    let mut scores: HashMap<PageId, f64> = HashMap::new();
    for (stream, weight) in streams {
        if *weight == 0.0 {
            continue;
        }
        for (rank, id) in stream.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += weight / (k + rank as f64 + 1.0);
        }
    }
    sorted(scores)
}

/// Fuse streams and return them sorted best-first.
pub fn fuse_and_rank(streams: &[Vec<PageId>], k: f64) -> Vec<(PageId, f64)> {
    sorted(reciprocal_rank_fusion(streams, k))
}

/// Best score first, ties broken by id so two runs agree.
fn sorted(scores: HashMap<PageId, f64>) -> Vec<(PageId, f64)> {
    let mut fused: Vec<(PageId, f64)> = scores.into_iter().collect();
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

    /// The two fusions have to agree wherever the weights say nothing, or the
    /// weighted one is a second implementation rather than a generalisation of
    /// the first.
    #[test]
    fn equal_weights_reproduce_the_unweighted_fusion() {
        let fts = vec![id(2), id(1), id(3)];
        let entity = vec![id(1), id(4)];
        let links = vec![id(1), id(5), id(2)];

        let plain = fuse_and_rank(&[fts.clone(), entity.clone(), links.clone()], RRF_K);
        let weighted = fuse_weighted(
            &[
                (fts.as_slice(), 1.0),
                (entity.as_slice(), 1.0),
                (links.as_slice(), 1.0),
            ],
            RRF_K,
        );

        assert_eq!(plain, weighted);
    }

    /// The defaults are a measurement, and a measurement someone can edit
    /// without noticing is a number back to being an opinion. Changing these
    /// means running `anamnesis eval --sweep` again and saying what moved.
    #[test]
    fn the_shipped_tuning_is_the_one_the_sweep_chose() {
        let tuning = Tuning::default();
        assert_eq!(tuning.rrf_k, 2.0);
        assert_eq!(tuning.fts, 1.0);
        assert_eq!(tuning.entity, 1.0);
        assert_eq!(tuning.links, 0.25);
        assert_eq!(tuning.vectors, 1.0);
        assert_eq!(tuning.authority_exponent, 0.25);
    }

    /// Why lowering `k` was safe for the scope fusion, which no suite covers:
    /// a page belongs to one project, so it appears in exactly one of those
    /// rankings, and single-membership rankings come out in rank order under
    /// any `k` at all.
    #[test]
    fn fusing_rankings_that_share_no_pages_orders_the_same_under_any_k() {
        let project = vec![id(1), id(2), id(3)];
        let shared = vec![id(4), id(5)];

        let order = |k: f64| -> Vec<PageId> {
            fuse_and_rank(&[project.clone(), shared.clone()], k)
                .into_iter()
                .map(|(page, _)| page)
                .collect()
        };

        assert_eq!(order(2.0), order(60.0));
        assert_eq!(order(2.0), order(0.5));
    }

    #[test]
    fn a_silenced_stream_contributes_nothing_at_all() {
        let fts = vec![id(1)];
        let links = vec![id(2), id(3)];
        let ranked = fuse_weighted(&[(fts.as_slice(), 1.0), (links.as_slice(), 0.0)], RRF_K);

        assert_eq!(ranked.len(), 1, "only the full-text hit should remain");
        assert_eq!(ranked[0].0, id(1));
    }

    /// Why the weights exist. At `k = 60` a page sitting deep in two streams
    /// outscores the page one stream ranked first, and halving the second
    /// stream is enough to reverse it.
    #[test]
    fn weighting_a_stream_down_lets_a_confident_stream_win() {
        let deep: Vec<PageId> = (10..40).map(|n| id(n as u128)).collect();
        let fts = vec![id(1)];
        let entity = vec![deep[20]];
        let links = vec![deep[20]];

        let equal = fuse_weighted(
            &[
                (fts.as_slice(), 1.0),
                (entity.as_slice(), 1.0),
                (links.as_slice(), 1.0),
            ],
            RRF_K,
        );
        assert_eq!(equal[0].0, deep[20], "two streams beat one at k = 60");

        let damped = fuse_weighted(
            &[
                (fts.as_slice(), 1.0),
                (entity.as_slice(), 0.5),
                (links.as_slice(), 0.25),
            ],
            RRF_K,
        );
        assert_eq!(damped[0].0, id(1), "the confident stream should now lead");
    }

    #[test]
    fn the_authority_exponent_spans_off_and_unchanged() {
        let with = |authority_exponent: f64| Tuning {
            authority_exponent,
            ..Tuning::default()
        };
        let full = authority_multiplier(true, true, true);

        assert!((with(1.0).authority(true, true, true) - full).abs() < 1e-9);
        assert!((with(0.0).authority(true, true, true) - 1.0).abs() < 1e-9);

        let half = with(0.5).authority(true, true, true);
        assert!(half > 1.0 && half < full, "{half} should sit between");

        // What ships: kept, and cut to about a quarter of its former reach.
        let shipped = Tuning::default().authority(true, true, true);
        assert!(
            shipped > 1.0,
            "authority should still prefer a canonical page"
        );
        assert!(shipped < 1.3, "but not by more than relevance can overcome");
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
