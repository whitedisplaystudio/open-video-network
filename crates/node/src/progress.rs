//! Live events for the local API.
//!
//! A download takes a while, and a UI that cannot say how far along it is
//! feels broken. These events are broadcast to anything watching the local
//! server-sent events stream.
//!
//! Everything here is about *content and connections* — never about viewing
//! behaviour. A fetch says which video is being downloaded and how far it has
//! got; nothing says whether it was watched, for how long, or what the local
//! model made of it.

use serde::Serialize;
use tokio::sync::broadcast;

/// How many events a slow consumer may fall behind before it starts missing
/// them. A UI that cannot keep up should skip, never hold the node back.
pub const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NodeEvent {
    #[serde(rename_all = "camelCase")]
    PeerConnected { peer_id: String },
    #[serde(rename_all = "camelCase")]
    PeerDisconnected { peer_id: String },
    #[serde(rename_all = "camelCase")]
    VideoDiscovered { cid: String, title: String },
    #[serde(rename_all = "camelCase")]
    FetchStarted {
        cid: String,
        total_chunks: usize,
        already_held: usize,
    },
    #[serde(rename_all = "camelCase")]
    FetchProgress {
        cid: String,
        completed_chunks: usize,
        total_chunks: usize,
        bytes_fetched: u64,
    },
    #[serde(rename_all = "camelCase")]
    FetchCompleted { cid: String, bytes_fetched: u64 },
    #[serde(rename_all = "camelCase")]
    FetchFailed { cid: String, error: String },
    #[serde(rename_all = "camelCase")]
    PublishStarted { file_name: String },
    #[serde(rename_all = "camelCase")]
    PublishCompleted { cid: String, title: String },
    #[serde(rename_all = "camelCase")]
    ReachabilityChanged { reachability: String },
    #[serde(rename_all = "camelCase")]
    RelayReserved { relay: String, address: String },
    #[serde(rename_all = "camelCase")]
    HolePunched { peer_id: String },
    /// The node is stopping. Listeners should close: an event stream never
    /// ends on its own, and a connection that never closes would hold up a
    /// graceful shutdown indefinitely.
    ShuttingDown,
}

/// Broadcasts [`NodeEvent`]s to every attached listener.
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<NodeEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self { sender }
    }

    /// Publish an event. Succeeds whether or not anyone is listening.
    pub fn emit(&self, event: NodeEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<NodeEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_listener_receives_what_is_emitted() {
        let bus = EventBus::new();
        let mut listener = bus.subscribe();
        bus.emit(NodeEvent::PeerConnected {
            peer_id: "12D3Koo".into(),
        });
        let received = listener.recv().await.unwrap();
        assert!(matches!(received, NodeEvent::PeerConnected { .. }));
    }

    #[test]
    fn emitting_with_no_listeners_is_fine() {
        let bus = EventBus::new();
        bus.emit(NodeEvent::FetchCompleted {
            cid: "bafy".into(),
            bytes_fetched: 1,
        });
    }

    #[test]
    fn events_serialise_with_a_type_tag_the_ui_can_switch_on() {
        let json = serde_json::to_value(NodeEvent::FetchProgress {
            cid: "bafy".into(),
            completed_chunks: 3,
            total_chunks: 10,
            bytes_fetched: 3000,
        })
        .unwrap();
        assert_eq!(json["type"], "fetchProgress");
        assert_eq!(json["completedChunks"], 3);
        assert_eq!(json["totalChunks"], 10);
    }

    #[test]
    fn no_event_carries_viewing_data() {
        // Every variant, rendered, must have only content and connection
        // fields. This is the same boundary as everywhere else.
        let samples = vec![
            NodeEvent::PeerConnected {
                peer_id: "p".into(),
            },
            NodeEvent::PeerDisconnected {
                peer_id: "p".into(),
            },
            NodeEvent::VideoDiscovered {
                cid: "c".into(),
                title: "t".into(),
            },
            NodeEvent::FetchStarted {
                cid: "c".into(),
                total_chunks: 1,
                already_held: 0,
            },
            NodeEvent::FetchProgress {
                cid: "c".into(),
                completed_chunks: 1,
                total_chunks: 1,
                bytes_fetched: 1,
            },
            NodeEvent::FetchCompleted {
                cid: "c".into(),
                bytes_fetched: 1,
            },
            NodeEvent::FetchFailed {
                cid: "c".into(),
                error: "e".into(),
            },
            NodeEvent::PublishStarted {
                file_name: "f".into(),
            },
            NodeEvent::PublishCompleted {
                cid: "c".into(),
                title: "t".into(),
            },
            NodeEvent::ShuttingDown,
            NodeEvent::ReachabilityChanged {
                reachability: "public".into(),
            },
            NodeEvent::RelayReserved {
                relay: "p".into(),
                address: "/ip4/192.0.2.1".into(),
            },
            NodeEvent::HolePunched {
                peer_id: "p".into(),
            },
        ];
        let permitted = [
            "type",
            "peerId",
            "cid",
            "title",
            "totalChunks",
            "alreadyHeld",
            "completedChunks",
            "bytesFetched",
            "error",
            "fileName",
            "reachability",
            "relay",
            "address",
        ];
        for event in samples {
            let value = serde_json::to_value(&event).unwrap();
            for key in value.as_object().unwrap().keys() {
                assert!(
                    permitted.contains(&key.as_str()),
                    "unexpected field {key:?} in {event:?}"
                );
            }
        }
    }
}
