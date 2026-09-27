//! Tunable weights for the V1 model.
//!
//! These are deliberately plain numbers in one visible place rather than
//! magic constants scattered through the scorer: section 28 makes
//! explainability a long-term goal, and that starts with the operator being
//! able to read the policy.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecommendationConfig {
    // --- how a viewing event becomes a preference signal
    /// Weight on the fraction of the video watched.
    pub ratio_weight: f64,
    /// Added when the viewer explicitly liked the video.
    pub like_bonus: f64,
    /// Added when the viewer watched to the end.
    pub completion_bonus: f64,
    /// Subtracted when the viewer deliberately skipped away.
    pub skip_penalty: f64,
    /// Half-life of a viewing event's influence, in days.
    pub preference_half_life_days: f64,

    // --- how a candidate video becomes a score
    /// Weight on the match between the video's tags and the preference vector.
    pub tag_weight: f64,
    /// Added when the creator is followed.
    pub following_weight: f64,
    /// Weight on how recent the video is.
    pub freshness_weight: f64,
    /// Half-life of the freshness term, in days.
    pub freshness_half_life_days: f64,
    /// Added for videos whose tags are all unknown to the model, so a new
    /// interest can still surface. Without this the feed converges on
    /// whatever was watched first.
    pub discovery_weight: f64,
    /// Subtracted for a video already watched.
    pub rewatch_penalty: f64,

    /// How many recent viewing events feed the model.
    pub history_window: usize,
    /// How many recent videos are scored as candidates.
    pub candidate_pool: usize,
}

impl Default for RecommendationConfig {
    fn default() -> Self {
        Self {
            ratio_weight: 1.0,
            like_bonus: 0.75,
            completion_bonus: 0.25,
            skip_penalty: 0.75,
            preference_half_life_days: 30.0,

            tag_weight: 1.0,
            following_weight: 0.5,
            freshness_weight: 0.2,
            freshness_half_life_days: 14.0,
            discovery_weight: 0.1,
            rewatch_penalty: 0.5,

            history_window: 2_000,
            candidate_pool: 500,
        }
    }
}
