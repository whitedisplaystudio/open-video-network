//! On-device recommendation (sections 27 and 28).
//!
//! The entire model is a vector of tag weights derived from what this user
//! watched, computed here and stored in this user's SQLite file. There is no
//! recommendation server, no upload, and no type in this crate that can be
//! serialised onto the network: [`Recommendation`], [`Contribution`] and
//! [`PreferenceModel`] deliberately have no `Serialize`.
//!
//! The model is also not a black box. Every score comes back with the list of
//! contributions that produced it, so `ourvideo recommendation explain` can
//! show the user exactly why a video was suggested.

mod config;
mod engine;
mod model;

pub use config::RecommendationConfig;
pub use engine::Engine;
pub use model::{Contribution, PreferenceModel, Recommendation};

#[derive(Debug, thiserror::Error)]
pub enum RecommendationError {
    #[error(transparent)]
    Database(#[from] ovn_database::DatabaseError),
}

pub type Result<T> = std::result::Result<T, RecommendationError>;

/// Seconds in a day, as a float. Ages are measured in days throughout.
pub(crate) const SECS_PER_DAY: f64 = 86_400.0;

/// `0.5^(age / half_life)`, clamped so a clock skew cannot produce a weight
/// above 1.
pub(crate) fn decay(age_days: f64, half_life_days: f64) -> f64 {
    if half_life_days <= 0.0 {
        return 1.0;
    }
    0.5f64.powf(age_days.max(0.0) / half_life_days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_halves_at_the_half_life() {
        assert!((decay(0.0, 30.0) - 1.0).abs() < 1e-9);
        assert!((decay(30.0, 30.0) - 0.5).abs() < 1e-9);
        assert!((decay(60.0, 30.0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_future_timestamp_does_not_score_above_the_present() {
        assert_eq!(decay(-100.0, 30.0), 1.0);
    }
}
