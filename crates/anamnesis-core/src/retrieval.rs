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

/// How many candidates each stream offers before fusion, unless a [`Tuning`]
/// says otherwise.
pub const STREAM_CANDIDATES: usize = 30;

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
    /// How much a page's vector counts for the share of the page it was
    /// embedded from.
    ///
    /// A model with a fixed window embeds the start of a long page and returns
    /// an ordinary vector, and `page_embed_failures` records how much of the
    /// page that start was. Each page's vector contribution is multiplied by
    /// that share raised to this exponent: `0.0` ignores it, and every vector
    /// counts in full however little of its page it stands for; `1.0` makes a
    /// vector that read a quarter of its page count for a quarter.
    ///
    /// Scales what a rank contributes, not the rank itself: the stream still
    /// orders pages by how close their vectors are, because that closeness is
    /// a true fact about the part of the page that was read.
    pub vector_coverage: f64,
    /// Whether a long page is matched by its sections.
    ///
    /// A page longer than the embedding model's window is embedded twice: once
    /// whole, which the model reads to its window and no further, and once in
    /// sections it reads entirely ([`crate::embedding::page_sections`]). Off,
    /// the vector stream compares a question with the whole-page vector only,
    /// as it always has. On, a page's closeness is its closest section's, and
    /// a page with sections counts as read in full.
    ///
    /// A page that fits its window has no sections, so this changes nothing
    /// about it either way.
    pub vector_sections: bool,
    /// Weight of the abstract stream: the query vector against each page's
    /// one-line `abstract:`, embedded on its own.
    ///
    /// A fifth stream rather than another vector of the fourth. One line is a
    /// single vector however long its page, so it cannot reward a page for
    /// having many parts the way the best of a page's sections does, and a
    /// page without an abstract is absent from it rather than compared by its
    /// body a second time. Zero silences it, and the stream is not even run.
    pub abstracts: f64,
    /// Exponent applied to [`authority_multiplier`]. `1.0` leaves it as it is,
    /// `0.0` switches it off, and anything between softens it. Standing says
    /// which place a page counts from ([`fuse_standing`]), so this is how far
    /// up its own ranking a distilled page may move, not what its score is
    /// multiplied by.
    pub authority_exponent: f64,
    /// How far down a session page that records almost nothing counts from.
    ///
    /// Asking memory a question leaves a session page that says the question,
    /// so the echoes of a question multiply while the answer stays one page.
    /// On 2026-09-22, in this project's own memory, `KIRLANGIC-47` — a marker
    /// one decision defines — was in six pages, five of them sessions where
    /// somebody had asked what it was, and the decision had fallen behind
    /// them. A record of a question is not an answer to it.
    pub thin_record_divisor: f64,
    /// How short a session page has to be to count as one of those records.
    ///
    /// In characters of body, an absolute number rather than a share of the
    /// corpus, for the reason the recall gate is absolute: a baseline needs a
    /// corpus to be a baseline. The pages it is drawn between are real — the
    /// question-only sessions in this memory run 550 to 1000 characters, and
    /// the session pages that answer something run past 1600.
    pub thin_record_chars: usize,
    /// How much of an entity's name the query has to say for it to match.
    ///
    /// `1.0` requires every token of the name — what the entity stream has
    /// always done. Lower admits partial matches, ranked by how complete they
    /// are, so a name half said sits below one said in full rather than beside
    /// it.
    pub entity_coverage: f64,
    /// How deep each stream goes before its ranking is fused.
    ///
    /// Not a page limit: the caller's limit still decides how many hits come
    /// back. This is how many candidates each stream is allowed to offer, and
    /// it cuts both ways — a shallow pool cannot answer with a page no stream
    /// rated highly, and a deep one lets three streams' also-rans outvote one
    /// stream's favourite.
    pub candidates: usize,
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
            // Off, and measured off. The argument for it was that a vector
            // standing for a quarter of its page should count for a quarter,
            // and `long` — the suite whose answers sit past the window — said
            // the opposite on 2026-09-13. Under `--embed`, exponent 1 took it
            // from hit@1 0.312 / MRR 0.414 to 0.188 / 0.301, and 0.5 to
            // 0.188 / 0.309: seven questions and six lost ground, none gained,
            // and every one that moved was answered by a long page.
            //
            // So a partial vector was helping its own page, not hurting it.
            // What costs `long` its answers (removing the stream entirely
            // gains six questions) is the full-weight vote for short pages
            // the model read whole — which this cannot touch, and which
            // scaling the long pages down only makes relatively stronger.
            // The three suites of short pages cannot see the knob at all:
            // every page fits, and a whole page scales by one.
            vector_coverage: 0.0,
            // Off, because measuring it found a trade rather than a gain.
            // Every long page in `long` is embedded in sections either way;
            // this only decides whether a query compares them. Under
            // `--embed`, on took `long` from MRR 0.414 to 0.469 with hit@1
            // level at 0.312: five questions gained, every one answered by a
            // long page and four of them paraphrase or symptom questions, and
            // three lost. One loss is a guard, `reset
            // cause`, 1 → 2, which is the cost the guards are there to show:
            // a page's closeness is the best of all its vectors, so a long
            // page can only get closer to every question, including the ones
            // a short page answers. The other two are deep questions, one of
            // which (`what has to happen before the boot report goes out`)
            // fell out of the results entirely. The three suites of short
            // pages did not move, as they cannot.
            //
            // Nor is on better than no vectors at all, which scores `long` at
            // 0.500 / 0.562. Sections made a long page's vector stand for the
            // page, which is what the coverage measurement said was needed,
            // and it was not enough on its own.
            vector_sections: false,
            // Off, and measured off. ai-memory's #672 found hit@1 0.609 to
            // 0.746 with this stream and a query router together, on a wiki
            // whose abstracts came from somewhere other than its own
            // consolidator, under an eight-billion-parameter embedder. Here, on
            // 2026-09-13, `long` with abstracts from two writers that never saw
            // its questions (gemini-3.6-flash and a local qwen2.5:7b) went from
            // 0.312 / 0.414 to 0.312 / 0.417 and 0.250 / 0.417 — trades, not
            // gains — and replacing the body vectors with abstracts scored
            // below removing vectors altogether under both. The sets are in
            // `docs/measurements/`; `docs/DIRECTION.md` has the table.
            abstracts: 0.0,
            // Three quarters, in rank space: an ordinary decision counts from
            // 1.36 places higher, a pinned canonical one from 1.89. It was a
            // quarter while standing multiplied the score, where it decided
            // nothing at all — the largest multiplier the wiki could give a
            // page was 1.24x and the gap between the first two places is
            // 1.33x. Measured over this project's 140 pages and the 36
            // questions in `questions/live-memory.toml`: MRR 0.706 → 0.766
            // and hit@1 0.611 → 0.722 with the divisor below, every fixture
            // suite at or above its floor, and `adversarial` — the suite no
            // knob is tuned against — unchanged at 0.969. Alone, without the
            // divisor, three quarters takes `adversarial` to 0.938, because
            // the trap it was written around is a session page.
            authority_exponent: 0.75,
            // Twice, and a page of 1500 characters or fewer. Measured on the
            // same 36 questions: with authority alone the real corpus reached
            // 0.736 and the untuned suite fell; with both it reaches 0.766 and
            // nothing falls. Demoting every session page instead scored 0.749
            // and took `long`, five of whose sixteen answers are sessions,
            // from 0.562 to 0.528: it is the thin record that is not an
            // answer, not the session page.
            thin_record_divisor: 2.0,
            thin_record_chars: 1500,
            // Thirty, unchanged — and the one knob whose measurement came
            // back empty. At the tuning above, ten, thirty and a hundred and
            // twenty score identically on both corpora. Depth only mattered
            // where the rest of the fusion was wrong: at `k = 60` with the
            // link stream and authority at full strength, a shallower pool
            // scored better, because fewer also-rans were available to outvote
            // the stream that had the answer. Fixing the fusion removed the
            // reason to cut it.
            //
            // Read with the corpora in mind. They are ten and twenty-two
            // pages, so thirty and a hundred and twenty are the same request
            // twice over; nothing here can tell them apart, and a suite large
            // enough to would have to be larger than the depth it is
            // measuring.
            // All of a name, still. The stream was written this way on the
            // argument that a two-word name answering one word would drown
            // the streams it is fused with, and the sweep agrees without
            // qualification: across two thousand comparisons, admitting
            // partial matches was never once better and usually worse — at
            // this tuning it costs the crowded suite 0.967 to 0.889. The rule
            // was right, and is now measured rather than argued.
            entity_coverage: 1.0,
            candidates: STREAM_CANDIDATES,
        }
    }
}

impl Tuning {
    /// The stream weights in the order the streams are fused.
    pub fn weights(&self) -> [f64; 5] {
        [
            self.fts,
            self.entity,
            self.links,
            self.vectors,
            self.abstracts,
        ]
    }

    /// What a page's vector contribution is multiplied by, for a vector that
    /// was embedded from `coverage` of its page.
    ///
    /// `coverage` is clamped to `[0, 1]`; a page that fit its window whole is
    /// `1.0` and scales by exactly one under any exponent.
    pub fn vector_scale(&self, coverage: f64) -> f64 {
        coverage.clamp(0.0, 1.0).powf(self.vector_coverage)
    }

    /// What a page's standing in the wiki is worth, as a factor on the place
    /// it counts from.
    ///
    /// Kept here rather than at the call site because the exponent only means
    /// anything against the multiplier it modifies, and separating them is how
    /// one of them silently stops being applied. `thin_record` says the page
    /// is a session that recorded almost nothing — see [`Tuning::thin_record`].
    pub fn standing(
        &self,
        pinned: bool,
        canonical: bool,
        authoritative_namespace: bool,
        thin_record: bool,
    ) -> f64 {
        let standing = authority_multiplier(pinned, canonical, authoritative_namespace)
            .powf(self.authority_exponent);
        if thin_record {
            standing / self.thin_record_divisor
        } else {
            standing
        }
    }

    /// Whether a page is a session that recorded almost nothing: in the
    /// `sessions` namespace and shorter than [`Tuning::thin_record_chars`].
    pub fn thin_record(&self, sessions_namespace: bool, body_chars: usize) -> bool {
        sessions_namespace && body_chars <= self.thin_record_chars
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
    let scaled: Vec<(&[PageId], f64, &[f64])> = streams
        .iter()
        .map(|(stream, weight)| (*stream, *weight, &[][..]))
        .collect();
    fuse_scaled(&scaled, k)
}

/// Fuse weighted streams in which each page's contribution can also be scaled
/// on its own, best-first.
///
/// `scales[i]` multiplies what the page at rank `i` contributes; a stream whose
/// scales are empty contributes in full. The one stream that uses this is the
/// vector stream, where a page embedded from part of itself counts for part —
/// see [`Tuning::vector_coverage`].
pub fn fuse_scaled(streams: &[(&[PageId], f64, &[f64])], k: f64) -> Vec<(PageId, f64)> {
    fuse_standing(streams, k, &HashMap::new())
}

/// [`fuse_scaled`], with each page's standing deciding the place it counts
/// from.
///
/// A page with standing `s` contributes what the page at `rank / s` would:
/// standing moves a page up the list by a factor, rather than multiplying a
/// score it cannot move. That difference is the whole point. At `k = 2` the
/// gap between the first two places is 1.33x and between the fifth and sixth
/// 1.14x, while the largest multiplier the wiki can give a page — pinned,
/// canonical, in an authoritative namespace — is 1.24x at the exponent that
/// ships, and an ordinary decision's is 1.11x. Multiplied into the score it
/// could not cross the top of its own ranking: on 2026-09-22, against this
/// project's own memory, the decision holding an answer sat third behind two
/// session pages that recorded only somebody asking the question, and
/// switching authority off changed no order at all.
pub fn fuse_standing(
    streams: &[(&[PageId], f64, &[f64])],
    k: f64,
    standing: &HashMap<PageId, f64>,
) -> Vec<(PageId, f64)> {
    let mut scores: HashMap<PageId, f64> = HashMap::new();
    for (stream, weight, scales) in streams {
        if *weight == 0.0 {
            continue;
        }
        for (rank, id) in stream.iter().enumerate() {
            let scale = scales.get(rank).copied().unwrap_or(1.0);
            let standing = standing
                .get(id)
                .copied()
                .unwrap_or(1.0)
                .max(f64::MIN_POSITIVE);
            let place = (rank as f64 + 1.0) / standing;
            *scores.entry(*id).or_insert(0.0) += weight * scale / (k + place);
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

/// What a page's standing in the wiki is worth, as a factor on the place it
/// counts from in every stream that found it ([`fuse_standing`]).
///
/// Not an independent retriever: a page no stream found stays absent however
/// authoritative it is. Only pages some stream already returned move within
/// the ranking.
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
        assert_eq!(tuning.authority_exponent, 0.75);
        assert_eq!(tuning.thin_record_divisor, 2.0);
        assert_eq!(tuning.thin_record_chars, 1500);
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

    /// A scale of one everywhere is no scale at all, so the generalisation has
    /// to agree with the fusion it generalises.
    #[test]
    fn unit_scales_reproduce_the_weighted_fusion() {
        let fts = vec![id(2), id(1), id(3)];
        let vectors = vec![id(3), id(2)];

        let weighted = fuse_weighted(&[(fts.as_slice(), 1.0), (vectors.as_slice(), 1.0)], RRF_K);
        let scaled = fuse_scaled(
            &[
                (fts.as_slice(), 1.0, &[][..]),
                (vectors.as_slice(), 1.0, &[1.0, 1.0][..]),
            ],
            RRF_K,
        );

        assert_eq!(weighted, scaled);
    }

    /// What the scale is for: the vector stream's favourite was read from a
    /// quarter of itself, and counting it for a quarter lets the page full
    /// text put first keep its place.
    #[test]
    fn a_page_scaled_down_in_one_stream_loses_the_tie_it_won_there() {
        let fts = vec![id(1), id(2)];
        let vectors = vec![id(2), id(1)];

        let whole = fuse_scaled(
            &[
                (fts.as_slice(), 1.0, &[][..]),
                (vectors.as_slice(), 1.5, &[1.0, 1.0][..]),
            ],
            RRF_K,
        );
        assert_eq!(whole[0].0, id(2), "a heavier vector stream decides the tie");

        let partial = fuse_scaled(
            &[
                (fts.as_slice(), 1.0, &[][..]),
                (vectors.as_slice(), 1.5, &[0.25, 1.0][..]),
            ],
            RRF_K,
        );
        assert_eq!(partial[0].0, id(1), "a quarter of a vector does not");
    }

    #[test]
    fn the_coverage_exponent_spans_off_and_proportional() {
        let with = |vector_coverage: f64| Tuning {
            vector_coverage,
            ..Tuning::default()
        };

        assert_eq!(
            with(0.0).vector_scale(0.25),
            1.0,
            "off counts every vector in full"
        );
        assert_eq!(with(1.0).vector_scale(0.25), 0.25);
        assert!((with(0.5).vector_scale(0.25) - 0.5).abs() < 1e-12);

        // A page that fit is untouched at any exponent, and a coverage outside
        // `[0, 1]` — a row nothing here wrote — is read as the nearest end.
        assert_eq!(with(1.0).vector_scale(1.0), 1.0);
        assert_eq!(with(1.0).vector_scale(7.0), 1.0);
        assert_eq!(with(1.0).vector_scale(-1.0), 0.0);
    }

    #[test]
    fn the_authority_exponent_spans_off_and_unchanged() {
        let with = |authority_exponent: f64| Tuning {
            authority_exponent,
            ..Tuning::default()
        };
        let full = authority_multiplier(true, true, true);

        assert!((with(1.0).standing(true, true, true, false) - full).abs() < 1e-9);
        assert!((with(0.0).standing(true, true, true, false) - 1.0).abs() < 1e-9);

        let half = with(0.5).standing(true, true, true, false);
        assert!(half > 1.0 && half < full, "{half} should sit between");
    }

    /// Standing has to be worth more than the gap between two places, or it
    /// decides nothing: that is what it was worth while it multiplied scores.
    #[test]
    fn what_ships_can_move_a_page_at_the_top_of_its_own_ranking() {
        let tuning = Tuning::default();
        let first_two_places = (tuning.rrf_k + 2.0) / (tuning.rrf_k + 1.0);

        let decision = tuning.standing(false, false, true, false);
        assert!(
            decision > first_two_places,
            "an ordinary decision ({decision}) cannot pass the page above it ({first_two_places})"
        );

        // And a session page that recorded almost nothing goes the other way,
        // by more than that same gap.
        let thin = tuning.standing(false, false, false, true);
        assert!(
            thin < 1.0 / first_two_places,
            "a thin session record ({thin}) should fall below the page under it"
        );
    }

    #[test]
    fn a_thin_record_is_a_short_session_page_and_nothing_else() {
        let tuning = Tuning::default();

        assert!(tuning.thin_record(true, 800));
        assert!(!tuning.thin_record(true, tuning.thin_record_chars + 1));
        assert!(!tuning.thin_record(false, 10), "only session pages");
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
