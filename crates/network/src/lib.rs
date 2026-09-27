//! The peer-to-peer layer: discovery, gossip and block transfer.
//!
//! One `libp2p` swarm runs in its own task. Everything else in the node holds
//! a [`Network`] handle and receives [`NetworkEvent`]s, so there is exactly
//! one place where connection state, rate limiting and protocol wiring live.
//!
//! Nothing here interprets content. Announcements arrive as raw bytes and are
//! verified by the node before they are believed; blocks arrive as raw bytes
//! and are checked against their content id before they are stored.

mod behaviour;
mod config;
mod event_loop;
mod handle;
mod rate_limit;

use std::time::Duration;

use libp2p::{gossipsub, noise, tcp, yamux};

use ovn_identity::Identity;
use ovn_protocol::{TOPIC_PROFILE_UPDATE, TOPIC_VIDEO_ANNOUNCE};

pub use behaviour::{BlockRequest, BlockResponse};
pub use config::{default_listen_addrs, NetworkConfig, DEFAULT_P2P_PORT};
pub use handle::{BlockResponder, DiscoverySource, Network, NetworkEvent, NetworkStatus};
pub use libp2p::{Multiaddr, PeerId};

use event_loop::EventLoop;

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("could not set up the network stack: {0}")]
    Setup(String),
    #[error("the network task has stopped")]
    Stopped,
    #[error("dial failed: {0}")]
    Dial(String),
    #[error("publish failed: {0}")]
    Publish(String),
    #[error("no peers are subscribed yet, so there is nobody to publish to")]
    NoPeers,
    #[error("DHT operation failed: {0}")]
    Dht(String),
    #[error("block transfer failed: {0}")]
    Transfer(String),
    #[error("the peer does not hold that block")]
    BlockNotFound,
    #[error("the peer refused to serve that block")]
    BlockRefused,
}

pub type Result<T> = std::result::Result<T, NetworkError>;

/// How many events may queue up before the oldest are dropped.
const EVENT_BUFFER: usize = 1024;
const COMMAND_BUFFER: usize = 256;

/// Build the swarm, start listening, and spawn the event loop.
///
/// Returns a handle, the event stream, and the task's join handle.
pub fn spawn(
    identity: &Identity,
    config: NetworkConfig,
) -> Result<(
    Network,
    tokio::sync::mpsc::Receiver<NetworkEvent>,
    tokio::task::JoinHandle<()>,
)> {
    let keypair = identity.libp2p_keypair();
    let local_peer_id = PeerId::from(keypair.public());

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| NetworkError::Setup(format!("tcp transport: {e}")))?
        .with_quic()
        .with_dns()
        .map_err(|e| NetworkError::Setup(format!("dns resolver: {e}")))?
        .with_behaviour(|keypair| {
            behaviour::Behaviour::new(keypair, &config).expect("behaviour construction")
        })
        .map_err(|e| NetworkError::Setup(format!("behaviour: {e}")))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_connection_timeout))
        .build();

    for topic in [TOPIC_VIDEO_ANNOUNCE, TOPIC_PROFILE_UPDATE] {
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&gossipsub::IdentTopic::new(topic))
            .map_err(|e| NetworkError::Setup(format!("subscribe to {topic}: {e}")))?;
    }

    let mut listening = 0usize;
    for addr in &config.listen_addrs {
        match swarm.listen_on(addr.clone()) {
            Ok(_) => listening += 1,
            // One address failing is normal — a machine without IPv6, say.
            Err(e) => tracing::warn!(%addr, error = %e, "could not listen on address"),
        }
    }
    if listening == 0 && !config.listen_addrs.is_empty() {
        return Err(NetworkError::Setup(
            "could not listen on any of the configured addresses".to_string(),
        ));
    }

    let (command_tx, command_rx) = tokio::sync::mpsc::channel(COMMAND_BUFFER);
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(EVENT_BUFFER);

    for addr in &config.bootstrap_addrs {
        if let Some(peer) = event_loop::extract_peer_id(addr) {
            swarm.behaviour_mut().kad.add_address(&peer, addr.clone());
        }
        if let Err(e) = swarm.dial(addr.clone()) {
            tracing::warn!(%addr, error = %e, "could not dial bootstrap address");
        }
    }

    let event_loop = EventLoop::new(swarm, config, command_rx, command_tx.clone(), event_tx);
    let task = tokio::spawn(event_loop.run());

    tracing::info!(peer_id = %local_peer_id, "network started");
    Ok((Network::new(command_tx, local_peer_id), event_rx, task))
}

/// A short grace period for the swarm to finish listening before a caller
/// reads back its addresses.
pub const LISTEN_SETTLE: Duration = Duration::from_millis(200);

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_config() -> NetworkConfig {
        NetworkConfig {
            // Port 0: let the OS pick, so tests never collide with a running
            // node or with each other.
            listen_addrs: vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()],
            enable_mdns: false,
            ..Default::default()
        }
    }

    async fn start() -> (
        Network,
        tokio::sync::mpsc::Receiver<NetworkEvent>,
        tokio::task::JoinHandle<()>,
        Multiaddr,
    ) {
        let identity = Identity::generate();
        let (network, mut events, task) = spawn(&identity, test_config()).unwrap();
        let addr = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(NetworkEvent::Listening(addr)) = events.recv().await {
                    return addr;
                }
            }
        })
        .await
        .expect("a listen address within five seconds");
        let full = addr.with(libp2p::multiaddr::Protocol::P2p(network.local_peer_id()));
        (network, events, task, full)
    }

    #[tokio::test]
    async fn a_node_starts_and_reports_itself() {
        let (network, _events, task, addr) = start().await;
        let status = network.status().await.unwrap();
        assert_eq!(status.local_peer_id, network.local_peer_id());
        assert!(!status.listen_addrs.is_empty());
        assert!(status.connected_peers.is_empty());
        assert!(addr.to_string().contains("quic-v1"));
        network.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn two_nodes_connect_with_no_third_party() {
        // Principle 1 in its smallest form: no bootstrap node, no server.
        let (a, mut a_events, a_task, a_addr) = start().await;
        let (b, _b_events, b_task, _) = start().await;

        let peer = b.dial(a_addr).await.unwrap();
        assert_eq!(peer, a.local_peer_id());

        let connected = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(NetworkEvent::PeerConnected(peer)) = a_events.recv().await {
                    return peer;
                }
            }
        })
        .await
        .expect("a connection event");
        assert_eq!(connected, b.local_peer_id());

        assert_eq!(a.status().await.unwrap().connected_peers.len(), 1);
        assert_eq!(b.status().await.unwrap().connected_peers.len(), 1);

        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        a_task.await.unwrap();
        b_task.await.unwrap();
    }

    #[tokio::test]
    async fn dialling_an_address_without_a_peer_id_is_an_error_not_a_hang() {
        let (network, _events, task, _) = start().await;
        let addr: Multiaddr = "/ip4/127.0.0.1/udp/1/quic-v1".parse().unwrap();
        assert!(network.dial(addr).await.is_err());
        network.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn publishing_with_no_peers_reports_that_rather_than_failing_silently() {
        let (network, _events, task, _) = start().await;
        assert!(matches!(
            network.publish_announcement(b"payload".to_vec()).await,
            Err(NetworkError::NoPeers)
        ));
        network.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_handle_on_a_stopped_network_reports_that_it_stopped() {
        let (network, _events, task, _) = start().await;
        network.shutdown().await.unwrap();
        task.await.unwrap();
        assert!(matches!(network.status().await, Err(NetworkError::Stopped)));
    }
}
