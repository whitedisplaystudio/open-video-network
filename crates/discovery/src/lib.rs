//! Joining the network from a link (sections 14 and 15).
//!
//! Principle 3 says a user should never have to know what a multiaddr is. The
//! whole of `ourvideo peer add` is here: take whatever the user pasted, turn
//! it into a verified [`NodeDescriptor`], and hand back addresses to dial.
//!
//! The URL is an entrance, not a dependency. Once the node has met one peer it
//! learns others through the DHT, and the site it came from can vanish.

mod fetch;
mod link;

use libp2p::Multiaddr;

use ovn_protocol::NodeDescriptor;

pub use fetch::{fetch_descriptor, DescriptorFetcher};
pub use link::{channel_link, parse_channel_link, parse_share_link, share_link, Target};

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("`{0}` is not a URL, a share link, or a peer address")]
    Unrecognised(String),
    #[error("share link is malformed: {0}")]
    MalformedLink(String),
    #[error("could not reach {url}: {source}")]
    Unreachable {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url} returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("the descriptor at {url} is {size} bytes, the limit is {limit}")]
    DescriptorTooLarge {
        url: String,
        size: usize,
        limit: usize,
    },
    #[error("the descriptor at {url} is not valid JSON: {message}")]
    MalformedDescriptor { url: String, message: String },
    #[error("the descriptor from {url} failed verification: {source}")]
    Untrusted {
        url: String,
        #[source]
        source: ovn_protocol::ProtocolError,
    },
    #[error("the descriptor lists no usable address")]
    NoAddresses,
    #[error("only http and https URLs are supported, not `{0}`")]
    UnsupportedScheme(String),
}

pub type Result<T> = std::result::Result<T, DiscoveryError>;

/// Dialable addresses for a verified descriptor.
///
/// Each address gets a `/p2p/<peer id>` component if it does not already have
/// one, so that a dial can be matched to the identity the descriptor claims.
/// Addresses that do not parse are skipped rather than failing the whole
/// descriptor: one bad entry should not make a node unreachable.
pub fn dial_addresses(descriptor: &NodeDescriptor) -> Result<Vec<Multiaddr>> {
    let peer_component = format!("/p2p/{}", descriptor.peer_id);
    let mut out = Vec::new();
    for address in &descriptor.addresses {
        let candidate = if address.contains("/p2p/") {
            address.clone()
        } else {
            format!("{address}{peer_component}")
        };
        match candidate.parse::<Multiaddr>() {
            Ok(addr) => {
                if !out.contains(&addr) {
                    out.push(addr);
                }
            }
            Err(e) => tracing::debug!(%address, error = %e, "skipping unparseable address"),
        }
    }
    if out.is_empty() {
        return Err(DiscoveryError::NoAddresses);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_identity::Identity;
    use ovn_protocol::Capability;

    fn descriptor(addresses: Vec<String>) -> NodeDescriptor {
        NodeDescriptor::sign(
            "node".into(),
            addresses,
            vec![Capability::bootstrap()],
            &Identity::generate(),
        )
        .unwrap()
    }

    #[test]
    fn a_peer_id_is_appended_to_bare_addresses() {
        let d = descriptor(vec!["/ip4/192.0.2.1/udp/4800/quic-v1".into()]);
        let addrs = dial_addresses(&d).unwrap();
        assert_eq!(addrs.len(), 1);
        assert!(addrs[0]
            .to_string()
            .ends_with(&format!("/p2p/{}", d.peer_id)));
    }

    #[test]
    fn an_address_that_already_names_the_peer_is_left_alone() {
        let d0 = descriptor(vec![]);
        let with_peer = format!("/ip4/192.0.2.1/udp/4800/quic-v1/p2p/{}", d0.peer_id);
        let d = NodeDescriptor::sign(
            "node".into(),
            vec![with_peer.clone()],
            vec![],
            &Identity::generate(),
        )
        .unwrap();
        let addrs = dial_addresses(&d).unwrap();
        assert_eq!(addrs[0].to_string(), with_peer);
    }

    #[test]
    fn unparseable_addresses_are_skipped_not_fatal() {
        let d = descriptor(vec![
            "not a multiaddr".into(),
            "/ip4/192.0.2.1/tcp/4800".into(),
        ]);
        assert_eq!(dial_addresses(&d).unwrap().len(), 1);
    }

    #[test]
    fn a_descriptor_with_no_usable_address_is_an_error() {
        let d = descriptor(vec!["nonsense".into()]);
        assert!(matches!(
            dial_addresses(&d),
            Err(DiscoveryError::NoAddresses)
        ));
    }

    #[test]
    fn duplicate_addresses_collapse() {
        let d = descriptor(vec![
            "/ip4/192.0.2.1/tcp/4800".into(),
            "/ip4/192.0.2.1/tcp/4800".into(),
        ]);
        assert_eq!(dial_addresses(&d).unwrap().len(), 1);
    }
}
