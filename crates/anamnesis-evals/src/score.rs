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

    #[test]
    fn an_empty_suite_scores_zero_rather_than_dividing_by_it() {
        assert_eq!(mean_reciprocal_rank(&[]), 0.0);
        assert_eq!(recall(&[]), 0.0);
    }
}
