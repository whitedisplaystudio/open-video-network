//! Building the preference model and scoring candidates.

use std::collections::{HashMap, HashSet};

use ovn_database::{Database, VideoRecord};
use ovn_protocol::now_secs;

use crate::model::{Contribution, PreferenceModel, Recommendation};
use crate::{decay, RecommendationConfig, Result, SECS_PER_DAY};

/// The recommendation engine. Holds no state of its own: everything it needs
/// is in the local database, and everything it produces goes back there.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engine {
    config: RecommendationConfig,
}

impl Engine {
    pub fn new(config: RecommendationConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &RecommendationConfig {
        &self.config
    }

    /// Derive a preference vector from local watch history.
    ///
    /// Each viewing event yields one signal, positive for engagement and
    /// negative for a skip, decayed by how long ago it happened. The signal is
    /// spread across the video's tags, so a video tagged with five things
    /// moves each of them less than a single-tag video would.
    pub fn build_preference_model(&self, db: &Database) -> Result<PreferenceModel> {
        let now = now_secs() as f64;
        let history = db.watch_history_with_tags(self.config.history_window)?;

        let mut totals: HashMap<String, f64> = HashMap::new();
        for (record, tags) in history {
            if tags.is_empty() {
                continue;
            }
            let ratio = record.ratio();
            let mut signal = self.config.ratio_weight * ratio;
            if record.liked {
                signal += self.config.like_bonus;
            }
            if record.completed {
                signal += self.config.completion_bonus;
            }
            if record.skipped {
                signal -= self.config.skip_penalty;
            }

            let age_days = ((now - record.watched_at as f64) / SECS_PER_DAY).max(0.0);
            let weight = decay(age_days, self.config.preference_half_life_days);
            let share = signal * weight / tags.len() as f64;
            for tag in tags {
                *totals.entry(tag).or_insert(0.0) += share;
            }
        }

        // Normalise to -1.0..=1.0 so the weights mean the same thing whether
        // the user has watched ten videos or ten thousand.
        let peak = totals.values().map(|v| v.abs()).fold(0.0f64, f64::max);
        if peak > 0.0 {
            for value in totals.values_mut() {
                *value /= peak;
            }
        }
        Ok(PreferenceModel::from_weights(totals))
    }

    /// Rebuild the model and store it, returning what was stored.
    pub fn refresh(&self, db: &Database) -> Result<PreferenceModel> {
        let model = self.build_preference_model(db)?;
        db.replace_tag_weights(&model.ranked())?;
        tracing::debug!(tags = model.len(), "rebuilt local preference model");
        Ok(model)
    }

    /// Load the stored model without recomputing it.
    pub fn stored_model(&self, db: &Database) -> Result<PreferenceModel> {
        let weights = db
            .tag_weights()?
            .into_iter()
            .map(|t| (t.tag, t.weight))
            .collect();
        Ok(PreferenceModel::from_weights(weights))
    }

    /// Score one video against a model. Public so that the explain API and
    /// the feed cannot drift apart: they call the same function.
    pub fn score(
        &self,
        video: &VideoRecord,
        model: &PreferenceModel,
        following: &HashSet<String>,
        watched: &HashSet<String>,
        now: f64,
    ) -> Recommendation {
        let mut contributions = Vec::new();

        if !video.tags.is_empty() {
            let per_tag = self.config.tag_weight / video.tags.len() as f64;
            for tag in &video.tags {
                let weight = model.weight(tag);
                if weight != 0.0 {
                    contributions.push(Contribution::tagged("tag", tag, weight * per_tag));
                }
            }
        }

        if following.contains(&video.creator) {
            contributions.push(Contribution::new("following", self.config.following_weight));
        }

        let age_days = ((now - video.created_at as f64) / SECS_PER_DAY).max(0.0);
        let freshness = decay(age_days, self.config.freshness_half_life_days);
        if freshness > 0.0 {
            contributions.push(Contribution::new(
                "freshness",
                freshness * self.config.freshness_weight,
            ));
        }

        // Exploration: if we know nothing about any of this video's tags, give
        // it a nudge so a new interest can break into the feed.
        let all_unknown = !video.tags.is_empty() && !video.tags.iter().any(|t| model.knows(t));
        if all_unknown || video.tags.is_empty() {
            contributions.push(Contribution::new("discovery", self.config.discovery_weight));
        }

        if watched.contains(&video.cid) {
            contributions.push(Contribution::new("rewatch", -self.config.rewatch_penalty));
        }

        let score = contributions.iter().map(|c| c.value).sum();
        Recommendation {
            cid: video.cid.clone(),
            title: video.title.clone(),
            score,
            contributions,
        }
    }

    /// The "For You" feed: every discovered video scored against this user's
    /// own model, highest first.
    pub fn recommend(&self, db: &Database, limit: usize) -> Result<Vec<Recommendation>> {
        let model = self.stored_model(db)?;
        let mut scored = self.score_candidates(db, &model)?;
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cid.cmp(&b.cid))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    /// Why this particular video would or would not be recommended.
    pub fn explain(
        &self,
        db: &Database,
        cid: &ovn_protocol::ContentId,
    ) -> Result<Option<Recommendation>> {
        let Some(video) = db.video(cid)? else {
            return Ok(None);
        };
        let model = self.stored_model(db)?;
        let following: HashSet<String> = db.following()?.into_iter().collect();
        let watched = db.watched_cids()?;
        Ok(Some(self.score(
            &video,
            &model,
            &following,
            &watched,
            now_secs() as f64,
        )))
    }

    fn score_candidates(
        &self,
        db: &Database,
        model: &PreferenceModel,
    ) -> Result<Vec<Recommendation>> {
        let candidates = db.videos(self.config.candidate_pool, 0)?;
        let following: HashSet<String> = db.following()?.into_iter().collect();
        let watched = db.watched_cids()?;
        let now = now_secs() as f64;
        Ok(candidates
            .iter()
            .map(|video| self.score(video, model, &following, &watched, now))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_database::{VideoUpsert, WatchEvent};
    use ovn_identity::Identity;
    use ovn_protocol::{to_cbor_vec, ContentId, NewVideo, VideoAnnouncement};

    struct Fixture {
        db: Database,
        creator: Identity,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                db: Database::open_in_memory().unwrap(),
                creator: Identity::generate(),
            }
        }

        fn publish(&self, seed: &str, tags: &[&str], age_days: f64) -> ContentId {
            self.publish_by(&self.creator, seed, tags, age_days)
        }

        fn publish_by(
            &self,
            creator: &Identity,
            seed: &str,
            tags: &[&str],
            age_days: f64,
        ) -> ContentId {
            let mut announcement = VideoAnnouncement::sign(
                NewVideo {
                    video_cid: Some(ContentId::from_dag_cbor(seed.as_bytes())),
                    title: seed.to_string(),
                    description: String::new(),
                    tags: tags.iter().map(|t| t.to_string()).collect(),
                    duration_secs: 600,
                    thumbnail_cid: None,
                    source_url: "https://videos.example/clip.mp4".to_string(),
                },
                creator,
            )
            .unwrap();
            announcement.created_at = now_secs() - (age_days * SECS_PER_DAY) as u64;
            let bytes = to_cbor_vec(&announcement).unwrap();
            self.db
                .upsert_video(VideoUpsert {
                    announcement: &announcement,
                    announcement_bytes: &bytes,
                    is_local: false,
                })
                .unwrap();
            announcement.video_cid
        }

        fn watch(&self, cid: ContentId, ratio: f64) {
            let duration = 600u32;
            self.db
                .record_watch(&WatchEvent {
                    cid,
                    watched_secs: (duration as f64 * ratio) as u32,
                    duration_secs: duration,
                    completed: ratio >= 0.99,
                    skipped: false,
                    liked: false,
                })
                .unwrap();
        }
    }

    #[test]
    fn with_no_history_the_model_is_empty_and_the_feed_is_still_useful() {
        let f = Fixture::new();
        f.publish("a", &["gaming"], 0.0);
        f.publish("b", &["music"], 40.0);
        let engine = Engine::default();
        assert!(engine.refresh(&f.db).unwrap().is_empty());

        let feed = engine.recommend(&f.db, 10).unwrap();
        assert_eq!(feed.len(), 2);
        // Nothing is known, so freshness and discovery decide: newest first.
        assert_eq!(feed[0].title, "a");
    }

    #[test]
    fn watching_gaming_raises_gaming_and_leaves_music_alone() {
        let f = Fixture::new();
        let game = f.publish("game", &["gaming"], 1.0);
        f.publish("song", &["music"], 1.0);
        f.watch(game, 1.0);

        let model = Engine::default().refresh(&f.db).unwrap();
        assert!(model.weight("gaming") > 0.0);
        assert_eq!(model.weight("music"), 0.0);
        assert!(!model.knows("music"));
    }

    #[test]
    fn two_users_with_different_habits_get_different_feeds() {
        // Acceptance test F, in miniature.
        let bob = Fixture::new();
        let carol = Fixture::new();
        let engine = Engine::default();

        for f in [&bob, &carol] {
            f.publish("game one", &["gaming"], 1.0);
            f.publish("game two", &["gaming"], 1.0);
            f.publish("song one", &["music"], 1.0);
            f.publish("song two", &["music"], 1.0);
        }

        bob.watch(ContentId::from_dag_cbor(b"game one"), 1.0);
        carol.watch(ContentId::from_dag_cbor(b"song one"), 1.0);
        engine.refresh(&bob.db).unwrap();
        engine.refresh(&carol.db).unwrap();

        let bob_top = &engine.recommend(&bob.db, 10).unwrap()[0];
        let carol_top = &engine.recommend(&carol.db, 10).unwrap()[0];
        // Each gets the *unwatched* video from the genre they engaged with.
        assert_eq!(bob_top.title, "game two");
        assert_eq!(carol_top.title, "song two");
    }

    #[test]
    fn a_skip_pushes_a_tag_negative() {
        let f = Fixture::new();
        let cid = f.publish("boring", &["lecture"], 1.0);
        f.db.record_watch(&ovn_database::WatchEvent {
            cid,
            watched_secs: 5,
            duration_secs: 600,
            completed: false,
            skipped: true,
            liked: false,
        })
        .unwrap();
        let model = Engine::default().refresh(&f.db).unwrap();
        assert!(model.weight("lecture") < 0.0, "{}", model.weight("lecture"));
    }

    #[test]
    fn a_like_counts_for_more_than_a_passive_view() {
        let f = Fixture::new();
        let loved = f.publish("loved", &["applause"], 1.0);
        let tolerated = f.publish("tolerated", &["shrug"], 1.0);
        for (cid, liked) in [(loved, true), (tolerated, false)] {
            f.db.record_watch(&ovn_database::WatchEvent {
                cid,
                watched_secs: 300,
                duration_secs: 600,
                completed: false,
                skipped: false,
                liked,
            })
            .unwrap();
        }
        let model = Engine::default().refresh(&f.db).unwrap();
        assert!(model.weight("applause") > model.weight("shrug"));
    }

    #[test]
    fn older_viewing_counts_for_less_than_recent_viewing() {
        let f = Fixture::new();
        let old = f.publish("old interest", &["photography"], 1.0);
        let new = f.publish("new interest", &["cooking"], 1.0);
        // Same engagement, but the photography view is four half-lives old.
        f.db.record_watch_at(
            &WatchEvent::new(old, 600, 600),
            now_secs() as i64 - (120.0 * SECS_PER_DAY) as i64,
        )
        .unwrap();
        f.db.record_watch(&WatchEvent::new(new, 600, 600)).unwrap();

        let model = Engine::default().refresh(&f.db).unwrap();
        assert!(
            model.weight("cooking") > model.weight("photography"),
            "cooking {} vs photography {}",
            model.weight("cooking"),
            model.weight("photography")
        );
    }

    #[test]
    fn a_video_already_watched_is_pushed_down_the_feed() {
        let f = Fixture::new();
        let seen = f.publish("seen", &["gaming"], 1.0);
        f.publish("unseen", &["gaming"], 1.0);
        f.watch(seen, 1.0);
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();

        let feed = engine.recommend(&f.db, 10).unwrap();
        assert_eq!(feed[0].title, "unseen");
        assert!(feed[1]
            .contributions
            .iter()
            .any(|c| c.factor == "rewatch" && c.value < 0.0));
    }

    #[test]
    fn following_a_creator_lifts_their_videos() {
        let f = Fixture::new();
        let other = Identity::generate();
        f.publish("from stranger", &["gaming"], 1.0);
        f.publish_by(&other, "from friend", &["gaming"], 1.0);
        f.db.set_following(&other.public_key(), true).unwrap();
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();
        assert_eq!(engine.recommend(&f.db, 10).unwrap()[0].title, "from friend");
    }

    #[test]
    fn a_fresh_video_outranks_an_identical_old_one() {
        let f = Fixture::new();
        f.publish("today", &["gaming"], 0.0);
        f.publish("last year", &["gaming"], 365.0);
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();
        assert_eq!(engine.recommend(&f.db, 10).unwrap()[0].title, "today");
    }

    #[test]
    fn a_video_on_an_unknown_topic_still_gets_a_discovery_nudge() {
        let f = Fixture::new();
        let known = f.publish("known", &["gaming"], 1.0);
        f.publish("brand new topic", &["pottery"], 1.0);
        f.watch(known, 1.0);
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();

        let feed = engine.recommend(&f.db, 10).unwrap();
        let pottery = feed.iter().find(|r| r.title == "brand new topic").unwrap();
        assert!(pottery
            .contributions
            .iter()
            .any(|c| c.factor == "discovery"));
        assert!(pottery.score > 0.0);
    }

    #[test]
    fn every_score_comes_with_its_reasons_and_they_sum_to_it() {
        let f = Fixture::new();
        let cid = f.publish("explained", &["gaming", "indie"], 2.0);
        f.watch(cid, 0.9);
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();

        let explained = engine.explain(&f.db, &cid).unwrap().unwrap();
        let sum: f64 = explained.contributions.iter().map(|c| c.value).sum();
        assert!((sum - explained.score).abs() < 1e-12);
        assert!(explained
            .contributions
            .iter()
            .any(|c| c.factor == "tag" && c.detail.as_deref() == Some("gaming")));
        assert!(explained
            .contributions
            .iter()
            .any(|c| c.factor == "freshness"));
    }

    #[test]
    fn explaining_an_unknown_video_is_none_not_an_error() {
        let f = Fixture::new();
        assert!(Engine::default()
            .explain(&f.db, &ContentId::from_dag_cbor(b"never seen"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn blocked_videos_never_reach_the_feed() {
        let f = Fixture::new();
        let blocked = f.publish("blocked", &["gaming"], 0.0);
        f.publish("allowed", &["gaming"], 1.0);
        f.db.block_cid(&blocked, "no thanks").unwrap();
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();
        let feed = engine.recommend(&f.db, 10).unwrap();
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].title, "allowed");
    }

    #[test]
    fn the_feed_is_deterministic_for_the_same_state() {
        let f = Fixture::new();
        for i in 0..5 {
            f.publish(&format!("v{i}"), &["gaming"], i as f64);
        }
        let engine = Engine::default();
        engine.refresh(&f.db).unwrap();
        let first = engine.recommend(&f.db, 10).unwrap();
        let second = engine.recommend(&f.db, 10).unwrap();
        assert_eq!(first, second);
    }
}
