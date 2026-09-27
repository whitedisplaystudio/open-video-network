//! The preference vector and the shape of an explained recommendation.
//!
//! None of these types implement `Serialize`. That is the enforcement of
//! section 32: a preference vector or a recommendation score has no encoding,
//! so it cannot be put in a protocol message.

use std::collections::HashMap;

/// Tag weights in `-1.0..=1.0`, learned from local viewing only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreferenceModel {
    weights: HashMap<String, f64>,
}

impl PreferenceModel {
    pub fn from_weights(weights: HashMap<String, f64>) -> Self {
        Self { weights }
    }

    pub fn weight(&self, tag: &str) -> f64 {
        self.weights.get(tag).copied().unwrap_or(0.0)
    }

    pub fn knows(&self, tag: &str) -> bool {
        self.weights.contains_key(tag)
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    pub fn len(&self) -> usize {
        self.weights.len()
    }

    /// Tags strongest first, for display and for persistence.
    pub fn ranked(&self) -> Vec<(String, f64)> {
        let mut pairs: Vec<(String, f64)> = self
            .weights
            .iter()
            .map(|(tag, weight)| (tag.clone(), *weight))
            .collect();
        pairs.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        pairs
    }
}

/// One term in a score, so a recommendation can be explained rather than
/// asserted.
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    /// `tag`, `following`, `freshness`, `discovery` or `rewatch`.
    pub factor: &'static str,
    /// The tag responsible, for `tag` contributions.
    pub detail: Option<String>,
    pub value: f64,
}

impl Contribution {
    pub fn new(factor: &'static str, value: f64) -> Self {
        Self {
            factor,
            detail: None,
            value,
        }
    }

    pub fn tagged(factor: &'static str, detail: impl Into<String>, value: f64) -> Self {
        Self {
            factor,
            detail: Some(detail.into()),
            value,
        }
    }
}

/// A scored candidate with the reasoning that produced the score.
#[derive(Clone, Debug, PartialEq)]
pub struct Recommendation {
    pub cid: String,
    pub title: String,
    pub score: f64,
    pub contributions: Vec<Contribution>,
}

impl Recommendation {
    /// The contributions, largest absolute effect first.
    pub fn ranked_contributions(&self) -> Vec<&Contribution> {
        let mut out: Vec<&Contribution> = self.contributions.iter().collect();
        out.sort_by(|a, b| {
            b.value
                .abs()
                .partial_cmp(&a.value.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_is_strongest_first_and_stable() {
        let model = PreferenceModel::from_weights(HashMap::from([
            ("music".into(), 0.31),
            ("gaming".into(), 0.91),
            ("indie".into(), 0.76),
            ("also".into(), 0.76),
        ]));
        let ranked = model.ranked();
        assert_eq!(ranked[0].0, "gaming");
        // Ties break alphabetically, so the output does not flicker.
        assert_eq!(ranked[1].0, "also");
        assert_eq!(ranked[2].0, "indie");
        assert_eq!(ranked[3].0, "music");
    }

    #[test]
    fn unknown_tags_weigh_nothing_but_are_distinguishable_from_zero() {
        let model = PreferenceModel::from_weights(HashMap::from([("seen".into(), 0.0)]));
        assert_eq!(model.weight("seen"), 0.0);
        assert_eq!(model.weight("unseen"), 0.0);
        assert!(model.knows("seen"));
        assert!(!model.knows("unseen"));
    }

    #[test]
    fn contributions_are_ranked_by_magnitude_including_negatives() {
        let rec = Recommendation {
            cid: "b".into(),
            title: "t".into(),
            score: 0.0,
            contributions: vec![
                Contribution::new("freshness", 0.12),
                Contribution::new("rewatch", -0.5),
                Contribution::tagged("tag", "gaming", 0.31),
            ],
        };
        let ranked = rec.ranked_contributions();
        assert_eq!(ranked[0].factor, "rewatch");
        assert_eq!(ranked[1].factor, "tag");
        assert_eq!(ranked[2].factor, "freshness");
    }
}
