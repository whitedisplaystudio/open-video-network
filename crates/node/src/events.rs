//! Reacting to the network.
//!
//! Everything arriving here is untrusted (section 31). Each handler verifies
//! before it believes: announcements and profiles must carry a valid
//! signature, and a block request is answered only from content we actually
//! hold and have not blocked.

use ovn_database::{PeerSource, VideoUpsert};
use ovn_network::{BlockResponse, DiscoverySource, NetworkEvent};
use ovn_protocol::{
    from_cbor_slice, ContentId, ProfileUpdate, VideoAnnouncement, MAX_GOSSIP_MESSAGE_SIZE,
};

use crate::node::Node;
use crate::progress::NodeEvent;

/// What happened to an inbound announcement. Returned so the behaviour can be
/// tested without a network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ingest {
    /// New to us and stored.
    Stored,
    /// Valid, but we already had it.
    Duplicate,
    /// Dropped, with the reason.
    Rejected(String),
}

pub(crate) async fn run(node: Node, mut events: tokio::sync::mpsc::Receiver<NetworkEvent>) {
    while let Some(event) = events.recv().await {
        handle(&node, event).await;
    }
    tracing::debug!("node event loop stopped");
}

async fn handle(node: &Node, event: NetworkEvent) {
    match event {
        NetworkEvent::Listening(addr) => {
            node.record_listen_addr(addr);
        }
        NetworkEvent::PeerDiscovered {
            peer,
            addresses,
            source,
        } => {
            let source = match source {
                DiscoverySource::Mdns => PeerSource::Mdns,
                DiscoverySource::Identify => PeerSource::Connected,
                DiscoverySource::Dht => PeerSource::Dht,
            };
            let addresses: Vec<String> = addresses.iter().map(|a| a.to_string()).collect();
            if let Err(e) =
                node.database()
                    .upsert_peer(&peer.to_base58(), &addresses, source, None, None)
            {
                tracing::warn!(error = %e, "could not record a discovered peer");
            }
        }
        NetworkEvent::PeerConnected(peer) => {
            let id = peer.to_base58();
            // Make sure the peer exists before marking it connected, so a peer
            // that dialled us is remembered too.
            let _ = node
                .database()
                .upsert_peer(&id, &[], PeerSource::Connected, None, None);
            if let Err(e) = node.database().mark_peer_connected(&id) {
                tracing::warn!(error = %e, "could not record a connection");
            }
            tracing::info!(peer = %id, "connected");
            node.emit(NodeEvent::PeerConnected { peer_id: id });
        }
        NetworkEvent::PeerDisconnected(peer) => {
            let id = peer.to_base58();
            tracing::info!(peer = %id, "disconnected");
            node.emit(NodeEvent::PeerDisconnected { peer_id: id });
        }
        NetworkEvent::GossipAnnouncement { from, data } => match ingest_announcement(node, &data) {
            Ingest::Stored => {}
            Ingest::Duplicate => {}
            Ingest::Rejected(reason) => {
                tracing::debug!(peer = %from, reason, "dropped an announcement");
            }
        },
        NetworkEvent::GossipProfile { from, data } => {
            if let Ingest::Rejected(reason) = ingest_profile(node, &data) {
                tracing::debug!(peer = %from, reason, "dropped a profile update");
            }
        }
        NetworkEvent::BlockRequested {
            peer,
            cid,
            responder,
        } => {
            let response = serve_block(node, &cid);
            tracing::trace!(peer = %peer, %cid, served = matches!(response, BlockResponse::Found(_)), "block request");
            responder.respond(response).await;
        }
    }
}

/// Verify and store an announcement received over gossip.
pub(crate) fn ingest_announcement(node: &Node, data: &[u8]) -> Ingest {
    if data.len() > MAX_GOSSIP_MESSAGE_SIZE {
        return Ingest::Rejected(format!("{} bytes is over the gossip limit", data.len()));
    }
    let announcement: VideoAnnouncement = match from_cbor_slice(data) {
        Ok(a) => a,
        Err(e) => return Ingest::Rejected(format!("malformed: {e}")),
    };
    // Structure and signature. An announcement that fails here is discarded,
    // never stored, never relayed onward by us.
    if let Err(e) = announcement.verify() {
        return Ingest::Rejected(format!("verification failed: {e}"));
    }

    let creator = match announcement.creator() {
        Ok(c) => c,
        Err(e) => return Ingest::Rejected(format!("bad creator key: {e}")),
    };
    match node.database().is_creator_blocked(&creator) {
        Ok(true) => return Ingest::Rejected("creator is blocked".to_string()),
        Ok(false) => {}
        Err(e) => return Ingest::Rejected(format!("database error: {e}")),
    }
    match node.database().is_cid_blocked(&announcement.video_cid) {
        Ok(true) => return Ingest::Rejected("content is blocked".to_string()),
        Ok(false) => {}
        Err(e) => return Ingest::Rejected(format!("database error: {e}")),
    }

    match node.database().upsert_video(VideoUpsert {
        announcement: &announcement,
        announcement_bytes: data,
        is_local: false,
    }) {
        Ok(true) => {
            tracing::info!(cid = %announcement.video_cid, title = %announcement.title, "discovered a video");
            node.emit(NodeEvent::VideoDiscovered {
                cid: announcement.video_cid.to_string(),
                title: announcement.title.clone(),
            });
            Ingest::Stored
        }
        Ok(false) => Ingest::Duplicate,
        Err(e) => Ingest::Rejected(format!("database error: {e}")),
    }
}

pub(crate) fn ingest_profile(node: &Node, data: &[u8]) -> Ingest {
    if data.len() > MAX_GOSSIP_MESSAGE_SIZE {
        return Ingest::Rejected(format!("{} bytes is over the gossip limit", data.len()));
    }
    let profile: ProfileUpdate = match from_cbor_slice(data) {
        Ok(p) => p,
        Err(e) => return Ingest::Rejected(format!("malformed: {e}")),
    };
    if let Err(e) = profile.verify() {
        return Ingest::Rejected(format!("verification failed: {e}"));
    }
    match node.database().upsert_creator(&profile) {
        Ok(true) => Ingest::Stored,
        Ok(false) => Ingest::Duplicate,
        Err(e) => Ingest::Rejected(format!("database error: {e}")),
    }
}

/// Answer a block request from local storage.
fn serve_block(node: &Node, cid: &ContentId) -> BlockResponse {
    // Content the user blocked locally is not served on to anyone else.
    if node.database().is_cid_blocked(cid).unwrap_or(false) {
        return BlockResponse::Refused;
    }
    match node.storage().try_get(cid) {
        Ok(Some(data)) => BlockResponse::Found(data),
        Ok(None) => BlockResponse::NotFound,
        Err(e) => {
            tracing::warn!(%cid, error = %e, "could not read a requested block");
            BlockResponse::NotFound
        }
    }
}
