//! Types for the local HTTP API.
//!
//! This module is the *only* place where local-only data — watch history,
//! preference weights, recommendation scores — is turned into something
//! serialisable, and it happens by hand, field by field.
//!
//! That is deliberate. Section 32 asks for the absence of a send mechanism,
//! not a setting. The database and recommendation types have no `Serialize`,
//! so nothing can encode them by accident; where a local GUI genuinely needs
//! the numbers, the conversion is written out here where a reviewer can see
//! it. These structures are served on loopback only and are never handed to
//! the network crate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use ovn_database::{CacheSummary, WatchRecord, WatchSummary};
use ovn_recommendation::{Contribution, PreferenceModel, Recommendation};

use crate::node::{AddPeerReport, FetchReport, NodeStatus, PublishReport};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDto {
    pub peer_id: String,
    pub public_key: String,
    pub node_name: String,
    pub started_at: u64,
    pub uptime_secs: u64,
    pub data_dir: PathBuf,
    pub listen_addrs: Vec<String>,
    pub connected_peers: usize,
    pub known_peers: i64,
    pub routing_table_peers: usize,
    pub known_videos: i64,
    pub local_videos: usize,
    pub providing: usize,
    pub cache: CacheSummary,
    pub cache_limit_bytes: u64,
    pub reachability: String,
    pub relays: usize,
}

impl From<NodeStatus> for StatusDto {
    fn from(s: NodeStatus) -> Self {
        Self {
            peer_id: s.peer_id,
            public_key: s.public_key,
            node_name: s.node_name,
            started_at: s.started_at,
            uptime_secs: s.uptime_secs,
            data_dir: s.data_dir,
            listen_addrs: s.listen_addrs,
            connected_peers: s.connected_peers,
            known_peers: s.known_peers,
            routing_table_peers: s.routing_table_peers,
            known_videos: s.known_videos,
            local_videos: s.local_videos,
            providing: s.providing,
            cache: s.cache,
            cache_limit_bytes: s.cache_limit_bytes,
            reachability: s.reachability,
            relays: s.relays,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishDto {
    pub cid: String,
    pub title: String,
    pub chunks: usize,
    pub total_bytes: u64,
    pub announced_to_network: bool,
}

impl From<PublishReport> for PublishDto {
    fn from(r: PublishReport) -> Self {
        Self {
            cid: r.video.cid,
            title: r.video.title,
            chunks: r.chunks,
            total_bytes: r.total_bytes,
            announced_to_network: r.announced_to_network,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchDto {
    pub cid: String,
    pub chunks_fetched: usize,
    pub chunks_already_held: usize,
    pub bytes_fetched: u64,
    pub providers_tried: usize,
    pub blocks_evicted: u64,
}

impl From<FetchReport> for FetchDto {
    fn from(r: FetchReport) -> Self {
        Self {
            cid: r.cid.to_string(),
            chunks_fetched: r.chunks_fetched,
            chunks_already_held: r.chunks_already_held,
            bytes_fetched: r.bytes_fetched,
            providers_tried: r.providers_tried,
            blocks_evicted: r.eviction.blocks_removed,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPeerDto {
    pub peer_id: String,
    pub node_name: String,
    pub addresses: Vec<String>,
    pub connected: bool,
}

impl From<AddPeerReport> for AddPeerDto {
    fn from(r: AddPeerReport) -> Self {
        Self {
            peer_id: r.peer_id,
            node_name: r.node_name,
            addresses: r.addresses,
            connected: r.connected,
        }
    }
}

// ---------------------------------------------------------------- local only

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasonDto {
    pub factor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub value: f64,
}

impl ReasonDto {
    fn from_contribution(c: &Contribution) -> Self {
        Self {
            factor: c.factor.to_string(),
            detail: c.detail.clone(),
            value: c.value,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendationDto {
    pub cid: String,
    pub title: String,
    pub score: f64,
    /// Why (section 28). The engine is not a black box.
    pub reasons: Vec<ReasonDto>,
}

impl RecommendationDto {
    pub fn from_recommendation(r: &Recommendation) -> Self {
        Self {
            cid: r.cid.clone(),
            title: r.title.clone(),
            score: r.score,
            reasons: r
                .ranked_contributions()
                .into_iter()
                .map(ReasonDto::from_contribution)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferenceDto {
    pub tag: String,
    pub weight: f64,
}

impl PreferenceDto {
    pub fn from_model(model: &PreferenceModel) -> Vec<Self> {
        model
            .ranked()
            .into_iter()
            .map(|(tag, weight)| Self { tag, weight })
            .collect()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchRecordDto {
    pub cid: String,
    pub watched_at: i64,
    pub watched_secs: u32,
    pub duration_secs: u32,
    pub ratio: f64,
    pub completed: bool,
    pub skipped: bool,
    pub liked: bool,
}

impl WatchRecordDto {
    pub fn from_record(r: &WatchRecord) -> Self {
        Self {
            cid: r.cid.clone(),
            watched_at: r.watched_at,
            watched_secs: r.watched_secs,
            duration_secs: r.duration_secs,
            ratio: r.ratio(),
            completed: r.completed,
            skipped: r.skipped,
            liked: r.liked,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSummaryDto {
    pub event_count: i64,
    pub distinct_videos: i64,
    pub total_watched_secs: i64,
}

impl From<WatchSummary> for WatchSummaryDto {
    fn from(s: WatchSummary) -> Self {
        Self {
            event_count: s.event_count,
            distinct_videos: s.distinct_videos,
            total_watched_secs: s.total_watched_secs,
        }
    }
}

// ------------------------------------------------------------------ requests

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPeerRequest {
    pub target: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRequest {
    pub path: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchRequest {
    pub cid: String,
    pub watched_secs: u32,
    #[serde(default)]
    pub duration_secs: u32,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub liked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRequest {
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRequest {
    pub display_name: String,
    #[serde(default)]
    pub bio: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recommendation_carries_its_reasons_largest_first() {
        let rec = Recommendation {
            cid: "bafy".into(),
            title: "t".into(),
            score: 0.5,
            contributions: vec![
                Contribution::new("freshness", 0.1),
                Contribution::tagged("tag", "gaming", 0.4),
            ],
        };
        let dto = RecommendationDto::from_recommendation(&rec);
        assert_eq!(dto.reasons[0].factor, "tag");
        assert_eq!(dto.reasons[0].detail.as_deref(), Some("gaming"));
        assert_eq!(dto.reasons[1].factor, "freshness");
    }

    #[test]
    fn list_queries_have_workable_defaults() {
        let query: ListQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(query.limit, 20);
        assert_eq!(query.offset, 0);
    }

    #[test]
    fn a_publish_request_needs_only_a_path() {
        let request: PublishRequest = serde_json::from_str(r#"{"path":"/tmp/a.mp4"}"#).unwrap();
        assert_eq!(request.path, PathBuf::from("/tmp/a.mp4"));
        assert!(request.title.is_none());
        assert!(request.tags.is_empty());
    }
}
