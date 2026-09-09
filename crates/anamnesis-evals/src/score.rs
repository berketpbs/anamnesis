//! Turning a ranking into a number.
//!
//! Pure, and separate from the running, because these are the definitions the
//! whole suite is argued over: what counts as an answer, and how much worse
//! third place is than first. Nothing here touches SQL, a corpus, or a clock.

/// Where a case's answer turned up, and how the case scored.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseScore {
    /// 1-based rank of the best relevant page, if one was returned at all.
    pub rank: Option<usize>,
    /// `1/rank`, or zero when nothing relevant came back.
    ///
    /// Reciprocal rather than a hit/miss count because an agent reads from the
    /// top and stops: an answer at rank 1 and an answer at rank 5 are not the
    /// same outcome, and averaging hits would call them one.
    pub reciprocal_rank: f64,
}

impl CaseScore {
    /// Whether anything relevant was returned at all.
    pub fn found(&self) -> bool {
        self.rank.is_some()
    }
}

/// Score one case's results against the pages that would have answered it.
///
/// `returned` is the ranking as retrieval produced it, best first. Only the
/// best-placed relevant page counts: a case that names three acceptable
/// answers is saying any of them would do, not that all three must appear.
pub fn score_case(returned: &[String], relevant: &[String]) -> CaseScore {
    let rank = returned
        .iter()
        .position(|path| relevant.iter().any(|wanted| wanted == path))
        .map(|index| index + 1);

    CaseScore {
        rank,
        reciprocal_rank: rank.map_or(0.0, |rank| 1.0 / rank as f64),
    }
}

/// Mean reciprocal rank over every case.
///
/// Zero for an empty suite rather than a division by zero: a suite with no
/// cases is refused at load, so this is only reachable through the API.
pub fn mean_reciprocal_rank(scores: &[CaseScore]) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    scores
        .iter()
        .map(|score| score.reciprocal_rank)
        .sum::<f64>()
        / scores.len() as f64
}

/// Share of cases whose answer appeared anywhere in the scored window.
///
/// Reported beside the mean reciprocal rank because they fail differently: a
/// suite can hold its recall while every answer slides from first place to
/// fifth, and that is a regression worth seeing.
pub fn recall(scores: &[CaseScore]) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    scores.iter().filter(|score| score.found()).count() as f64 / scores.len() as f64
}

/// Share of cases answered in first place.
///
/// The bluntest number here and the closest to what an agent is actually
/// handed: it reads the top hit and works from it. Worth reporting beside the
/// mean because the two saturate at different times — this repository's
/// `crowded` suite holds a mean of 0.967 with one question in fifteen answered
/// second, and the fifteenth is the one somebody notices.
pub fn hit_at_one(scores: &[CaseScore]) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    scores.iter().filter(|score| score.rank == Some(1)).count() as f64 / scores.len() as f64
}

/// Normalised discounted cumulative gain over the first `k` results.
///
/// Reported because it is the figure every other retrieval project publishes,
/// and a measurement nobody outside this repository can hold against their own
/// is only a measurement of our own past.
///
/// Be clear about what it means here, because the name carries an assumption
/// this suite does not make. A case's `relevant` list is a set of *acceptable*
/// answers — any one of them settles the question, which is why [`score_case`]
/// takes the best-placed and ignores the rest — so every case has exactly one
/// thing to find, and the ideal ranking is that thing in first place. With a
/// single relevant item the definition reduces to `1 / log2(1 + rank)`, which
/// is what this computes. The textbook form, accumulating gain over every
/// listed page, would report a case that returned precisely what it asked for
/// as a partial failure whenever the case named a second acceptable answer,
/// and five of the twenty-five cases shipped here do.
///
/// Its discount is gentler than the reciprocal rank's, which is the reason to
/// carry both. Second place halves `1/rank` and costs this a third; ninth place
/// leaves the reciprocal 0.11 of its scale and this 0.30 of its own. The gap
/// between two late ranks is much the same under either — what differs is
/// whether a suite's mean can still feel it.
pub fn ndcg_at(scores: &[CaseScore], k: usize) -> f64 {
    if scores.is_empty() || k == 0 {
        return 0.0;
    }
    scores
        .iter()
        .map(|score| match score.rank {
            Some(rank) if rank <= k => 1.0 / ((1 + rank) as f64).log2(),
            _ => 0.0,
        })
        .sum::<f64>()
        / scores.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn first_place_scores_one_and_nothing_scores_zero() {
        let hit = score_case(&paths(&["a.md", "b.md"]), &paths(&["a.md"]));
        assert_eq!(hit.rank, Some(1));
        assert_eq!(hit.reciprocal_rank, 1.0);

        let miss = score_case(&paths(&["b.md", "c.md"]), &paths(&["a.md"]));
        assert_eq!(miss.rank, None);
        assert_eq!(miss.reciprocal_rank, 0.0);
        assert!(!miss.found());
    }

    /// The reason for reciprocal rank rather than a hit count: an answer
    /// nobody scrolls to is not the same outcome as an answer at the top.
    #[test]
    fn a_lower_placed_answer_scores_less() {
        let third = score_case(&paths(&["x.md", "y.md", "a.md"]), &paths(&["a.md"]));
        assert_eq!(third.rank, Some(3));
        assert!((third.reciprocal_rank - 1.0 / 3.0).abs() < f64::EPSILON);
        assert!(third.found());
    }

    /// Several acceptable answers means any of them will do — the best-placed
    /// one is the score, not the first one the case happened to list.
    #[test]
    fn the_best_placed_acceptable_answer_is_the_one_that_counts() {
        let score = score_case(&paths(&["b.md", "a.md"]), &paths(&["a.md", "b.md"]));
        assert_eq!(score.rank, Some(1));
    }

    #[test]
    fn the_two_measures_disagree_when_answers_merely_slide_down() {
        let sharp = vec![
            score_case(&paths(&["a.md"]), &paths(&["a.md"])),
            score_case(&paths(&["b.md"]), &paths(&["b.md"])),
        ];
        let blunt = vec![
            score_case(&paths(&["x.md", "y.md", "a.md"]), &paths(&["a.md"])),
            score_case(&paths(&["x.md", "y.md", "b.md"]), &paths(&["b.md"])),
        ];

        assert_eq!(recall(&sharp), recall(&blunt), "both still find everything");
        assert!(
            mean_reciprocal_rank(&sharp) > mean_reciprocal_rank(&blunt),
            "sliding down the page is a regression the recall figure cannot see"
        );
    }

    /// The measure with the least give in it: second place counts for nothing,
    /// which is the point of having it beside a mean that would call it 0.5.
    #[test]
    fn only_first_place_is_a_hit() {
        let first = score_case(&paths(&["a.md", "b.md"]), &paths(&["a.md"]));
        let second = score_case(&paths(&["b.md", "a.md"]), &paths(&["a.md"]));
        let missing = score_case(&paths(&["b.md"]), &paths(&["a.md"]));

        assert_eq!(hit_at_one(std::slice::from_ref(&first)), 1.0);
        assert_eq!(hit_at_one(std::slice::from_ref(&second)), 0.0);
        assert_eq!(hit_at_one(std::slice::from_ref(&missing)), 0.0);
        assert_eq!(hit_at_one(&[first, second, missing]), 1.0 / 3.0);
    }

    /// The two numbers a case can produce, against the definition rather than
    /// against the implementation: first place is a full point, and third is
    /// exactly a half because `log2(4)` is 2.
    #[test]
    fn the_gain_is_one_at_the_top_and_a_half_at_third() {
        let first = score_case(&paths(&["a.md"]), &paths(&["a.md"]));
        assert_eq!(ndcg_at(&[first], 5), 1.0);

        let third = score_case(&paths(&["x.md", "y.md", "a.md"]), &paths(&["a.md"]));
        assert!((ndcg_at(&[third], 5) - 0.5).abs() < 1e-12);

        let missing = score_case(&paths(&["x.md"]), &paths(&["a.md"]));
        assert_eq!(ndcg_at(&[missing], 5), 0.0);
    }

    /// An answer outside the window is not an answer. `k` is the scored window
    /// rather than the length of what came back, so a report can say @5 and
    /// mean it even if a caller handed it a longer list.
    #[test]
    fn an_answer_past_k_earns_nothing() {
        let fourth = score_case(&paths(&["w.md", "x.md", "y.md", "a.md"]), &paths(&["a.md"]));
        assert!(ndcg_at(std::slice::from_ref(&fourth), 5) > 0.0);
        assert_eq!(ndcg_at(&[fourth], 3), 0.0);
    }

    /// Why both discounts are worth carrying, stated as the two facts that are
    /// actually true of them rather than as the one that sounds true.
    ///
    /// It is not that the gain separates deep ranks more widely — the gap
    /// between fifth place and ninth is 0.089 under the reciprocal and 0.086
    /// under the gain, which is the same gap. It is *where on the scale* that
    /// gap sits. By ninth place the reciprocal has spent 89% of its range and a
    /// suite's mean can barely feel the difference; the gain still has 30% of
    /// the scale under it, so an answer that is late but present goes on
    /// counting as present.
    #[test]
    fn the_gain_charges_less_for_being_late() {
        let second = score_case(&paths(&["x", "answer"]), &paths(&["answer"]));
        let ninth = score_case(
            &paths(&["a", "b", "c", "d", "e", "f", "g", "h", "answer"]),
            &paths(&["answer"]),
        );

        // Second place halves the reciprocal rank and costs the gain a third.
        assert_eq!(second.reciprocal_rank, 0.5);
        assert!((ndcg_at(&[second], 10) - 0.6309).abs() < 1e-4);

        // And what is left at ninth: a ninth of the scale against a third of it.
        // Written as bounds rather than as a literal, because the value there
        // is log10(2) to four places and a checked-in approximation of a named
        // constant is a lint rather than a measurement.
        let late = ndcg_at(std::slice::from_ref(&ninth), 10);
        assert!((ninth.reciprocal_rank - 0.1111).abs() < 1e-4);
        assert!((0.30..0.31).contains(&late), "{late}");
        assert!(
            late > ninth.reciprocal_rank * 2.0,
            "the measure that still reports a late answer as an answer"
        );
    }

    #[test]
    fn an_empty_suite_scores_zero_rather_than_dividing_by_it() {
        assert_eq!(mean_reciprocal_rank(&[]), 0.0);
        assert_eq!(recall(&[]), 0.0);
        assert_eq!(hit_at_one(&[]), 0.0);
        assert_eq!(ndcg_at(&[], 5), 0.0);
    }

    /// A window of zero results is not a division by zero either. Refused at
    /// load, so this is only reachable through the API — same as an empty
    /// suite, and it fails the same quiet way if it is not handled.
    #[test]
    fn a_window_of_nothing_scores_zero() {
        let first = score_case(&paths(&["a.md"]), &paths(&["a.md"]));
        assert_eq!(ndcg_at(&[first], 0), 0.0);
    }
}
