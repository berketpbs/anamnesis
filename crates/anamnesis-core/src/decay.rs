//! Retention scoring: how strongly a page resists being forgotten.
//!
//! Forgetting is a feature, not an accident. Without it the wiki accumulates
//! every session summary ever written and retrieval quality falls as the corpus
//! grows. The score combines two independent pressures:
//!
//! ```text
//! salience · e^(-λ · age_days)  +  σ · ln(1 + access_count) · e^(-μ · days_since_access)
//! ```
//!
//! The first term is *how important it was when written*, decaying with age.
//! The second is *how often it has proven useful since*, decaying with disuse.
//! A page written once and never read fades; a page read repeatedly stays,
//! even when old.

/// Tunable coefficients for [`retention_score`].
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecayParams {
    /// Age decay rate, `λ`. Larger forgets faster.
    pub lambda: f64,
    /// Weight of the access term, `σ`.
    pub sigma: f64,
    /// Disuse decay rate, `μ`.
    pub mu: f64,
}

impl DecayParams {
    /// Half-life of the age term, in days, under the default `λ`.
    pub const AGE_HALF_LIFE_DAYS: f64 = 30.0;

    /// Half-life of the access term, in days, under the default `μ`.
    pub const ACCESS_HALF_LIFE_DAYS: f64 = 14.0;
}

impl Default for DecayParams {
    fn default() -> Self {
        Self {
            lambda: std::f64::consts::LN_2 / Self::AGE_HALF_LIFE_DAYS,
            sigma: 0.5,
            mu: std::f64::consts::LN_2 / Self::ACCESS_HALF_LIFE_DAYS,
        }
    }
}

/// The observable facts a retention score is computed from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecayInputs {
    /// Importance assigned when the page was written. Typically `1.0`.
    pub salience: f64,
    /// Days since the page was written.
    pub age_days: f64,
    /// How many times the page has been retrieved.
    pub access_count: u32,
    /// Days since the most recent retrieval. Equal to `age_days` if never read.
    pub days_since_access: f64,
}

impl DecayInputs {
    /// Inputs for a page written `age_days` ago and never retrieved.
    pub fn unread(salience: f64, age_days: f64) -> Self {
        Self {
            salience,
            age_days,
            access_count: 0,
            days_since_access: age_days,
        }
    }
}

/// Score a page's resistance to being forgotten. Higher survives longer.
pub fn retention_score(inputs: DecayInputs, params: DecayParams) -> f64 {
    let age_term = inputs.salience * (-params.lambda * inputs.age_days.max(0.0)).exp();
    let access_term = params.sigma
        * f64::from(inputs.access_count).ln_1p()
        * (-params.mu * inputs.days_since_access.max(0.0)).exp();
    age_term + access_term
}

/// Whether a page should be swept, given a threshold.
///
/// Pinned pages are exempt and must never reach this function; that exemption
/// is a first-order retention control, not a scoring adjustment.
pub fn is_forgettable(inputs: DecayInputs, params: DecayParams, threshold: f64) -> bool {
    retention_score(inputs, params) < threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(inputs: DecayInputs) -> f64 {
        retention_score(inputs, DecayParams::default())
    }

    #[test]
    fn a_fresh_page_scores_its_full_salience() {
        let now = score(DecayInputs::unread(1.0, 0.0));
        assert!((now - 1.0).abs() < 1e-9);
    }

    #[test]
    fn age_halves_the_first_term_on_schedule() {
        let aged = score(DecayInputs::unread(
            1.0,
            DecayParams::AGE_HALF_LIFE_DAYS,
        ));
        assert!((aged - 0.5).abs() < 1e-9);
    }

    #[test]
    fn unread_pages_fade_monotonically() {
        let mut previous = f64::INFINITY;
        for days in [0.0, 10.0, 30.0, 90.0, 365.0] {
            let current = score(DecayInputs::unread(1.0, days));
            assert!(current < previous, "score rose at {days} days");
            previous = current;
        }
    }

    #[test]
    fn repeated_use_outweighs_age() {
        let old_and_unread = score(DecayInputs::unread(1.0, 180.0));
        let old_but_used = score(DecayInputs {
            salience: 1.0,
            age_days: 180.0,
            access_count: 20,
            days_since_access: 1.0,
        });
        assert!(old_but_used > old_and_unread);
        // And it beats a page written only a week ago that nobody reads.
        assert!(old_but_used > score(DecayInputs::unread(1.0, 7.0)));
    }

    #[test]
    fn the_access_term_itself_decays_with_disuse() {
        let inputs = |days_since_access| DecayInputs {
            salience: 1.0,
            age_days: 200.0,
            access_count: 20,
            days_since_access,
        };
        assert!(score(inputs(1.0)) > score(inputs(60.0)));
    }

    #[test]
    fn salience_scales_the_age_term() {
        assert!(score(DecayInputs::unread(2.0, 30.0)) > score(DecayInputs::unread(1.0, 30.0)));
    }

    #[test]
    fn sweeping_uses_the_same_score() {
        let params = DecayParams::default();
        let ancient = DecayInputs::unread(1.0, 3650.0);
        assert!(is_forgettable(ancient, params, 0.05));
        assert!(!is_forgettable(DecayInputs::unread(1.0, 0.0), params, 0.05));
    }

    #[test]
    fn negative_ages_are_clamped_rather_than_amplified() {
        // Clock skew between a hook host and the server must not manufacture
        // an immortal page.
        let skewed = score(DecayInputs::unread(1.0, -1000.0));
        assert!((skewed - 1.0).abs() < 1e-9);
    }
}
