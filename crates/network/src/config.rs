//! Network configuration.

use std::time::Duration;

use libp2p::Multiaddr;

/// Default QUIC/TCP port. Chosen away from the crowded low ports so that a
/// first run on a developer machine does not collide with something already
/// listening.
pub const DEFAULT_P2P_PORT: u16 = 4800;

#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// Addresses to listen on. Defaults to QUIC and TCP on all interfaces.
    pub listen_addrs: Vec<Multiaddr>,
    /// Announce ourselves on the local network, and listen for neighbours.
    pub enable_mdns: bool,
    /// Optional entry points. The network must work with this empty
    /// (Principle 1), so nothing here is required.
    pub bootstrap_addrs: Vec<Multiaddr>,
    /// Participate in the DHT as a server rather than only querying it.
    pub dht_server_mode: bool,
    /// Ask the router to forward our port, so we are reachable without a
    /// relay at all.
    pub enable_upnp: bool,
    /// Relay for peers that cannot be reached directly, when we can be.
    ///
    /// On by default: a network where only a few volunteers relay is a
    /// network with a dependency, which is exactly what Principle 1 forbids.
    /// A node that is itself unreachable never gets asked, so leaving this on
    /// costs nothing.
    pub enable_relay_server: bool,
    /// How many relays to hold a reservation with when we are unreachable.
    pub max_relay_reservations: usize,
    /// Addresses we know we are reachable on, for an operator who has
    /// forwarded a port themselves. Saves waiting to be told by other peers,
    /// and stops us taking a relay slot we do not need.
    pub external_addrs: Vec<Multiaddr>,
    /// Hard ceiling on established connections.
    pub max_connections: u32,
    /// Ceiling on connections from a single peer.
    pub max_connections_per_peer: u32,
    /// Gossip messages accepted from one peer per minute, before the peer is
    /// ignored for a while.
    pub gossip_rate_per_minute: u32,
    /// Block requests served to one peer per minute.
    pub block_request_rate_per_minute: u32,
    /// How long an idle connection is held open.
    pub idle_connection_timeout: Duration,
    /// How often to re-run a Kademlia bootstrap.
    pub bootstrap_interval: Duration,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addrs: default_listen_addrs(DEFAULT_P2P_PORT),
            enable_mdns: true,
            bootstrap_addrs: Vec::new(),
            dht_server_mode: true,
            enable_upnp: true,
            enable_relay_server: true,
            max_relay_reservations: 2,
            external_addrs: Vec::new(),
            max_connections: 256,
            max_connections_per_peer: 4,
            gossip_rate_per_minute: 240,
            block_request_rate_per_minute: 1_200,
            idle_connection_timeout: Duration::from_secs(60),
            bootstrap_interval: Duration::from_secs(300),
        }
    }
}

/// QUIC first (it needs no separate handshake round trip), TCP as a fallback
/// for networks that block UDP.
pub fn default_listen_addrs(port: u16) -> Vec<Multiaddr> {
    vec![
        format!("/ip4/0.0.0.0/udp/{port}/quic-v1")
            .parse()
            .expect("static multiaddr"),
        format!("/ip4/0.0.0.0/tcp/{port}")
            .parse()
            .expect("static multiaddr"),
        format!("/ip6/::/udp/{port}/quic-v1")
            .parse()
            .expect("static multiaddr"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_listen_addresses_cover_quic_and_tcp() {
        let addrs = default_listen_addrs(4800);
        assert_eq!(addrs.len(), 3);
        assert!(addrs.iter().any(|a| a.to_string().contains("quic-v1")));
        assert!(addrs.iter().any(|a| a.to_string().contains("/tcp/4800")));
    }

    #[test]
    fn a_default_config_needs_no_bootstrap_peer() {
        // Principle 1: the network must form without any operator-run node.
        assert!(NetworkConfig::default().bootstrap_addrs.is_empty());
    }

    #[test]
    fn a_default_node_helps_others_through_their_nat() {
        // If relaying were opt-in, the few who opted in would become
        // infrastructure the rest depended on.
        let config = NetworkConfig::default();
        assert!(config.enable_relay_server);
        assert!(config.enable_upnp);
        assert!(config.max_relay_reservations > 0);
    }
}
