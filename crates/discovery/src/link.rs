//! Parsing whatever the user pasted.

use libp2p::Multiaddr;

use ovn_protocol::{
    from_cbor_slice, to_cbor_vec, NodeDescriptor, MAX_DESCRIPTOR_SIZE, SHARE_LINK_SCHEME,
};

use crate::{DiscoveryError, Result};

/// What `ourvideo peer add <thing>` was given.
#[derive(Clone, Debug)]
pub enum Target {
    /// An `https://…` or `http://…` address to fetch a descriptor from.
    Url(String),
    /// An `ourvideo://…` link with the descriptor embedded.
    ShareLink(Box<NodeDescriptor>),
    /// A raw libp2p multiaddr, for people who do know what one is.
    Address(Multiaddr),
}

impl Target {
    /// Classify user input. Order matters: a share link and a URL are both
    /// "scheme://something", so the share link scheme is checked first.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(DiscoveryError::Unrecognised(input.to_string()));
        }
        if let Some(rest) = input.strip_prefix(&format!("{SHARE_LINK_SCHEME}://")) {
            return Ok(Self::ShareLink(Box::new(parse_share_link_body(rest)?)));
        }
        if input.starts_with('/') {
            return input
                .parse::<Multiaddr>()
                .map(Self::Address)
                .map_err(|_| DiscoveryError::Unrecognised(input.to_string()));
        }
        if let Some(scheme) = input.split("://").next().filter(|s| *s != input) {
            return match scheme {
                "http" | "https" => Ok(Self::Url(input.to_string())),
                other => Err(DiscoveryError::UnsupportedScheme(other.to_string())),
            };
        }
        // A bare hostname is the common case in the design's example, so treat
        // `video.example.jp` as `https://video.example.jp`.
        if looks_like_a_host(input) {
            return Ok(Self::Url(format!("https://{input}")));
        }
        Err(DiscoveryError::Unrecognised(input.to_string()))
    }
}

fn looks_like_a_host(input: &str) -> bool {
    !input.contains(char::is_whitespace)
        && input.contains('.')
        && input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._:/".contains(c))
}

/// Encode a descriptor as a shareable `ourvideo://` link.
///
/// The descriptor travels inside the link, so a link works with no web server
/// at all — which is what Principle 1 needs from a sharing mechanism.
pub fn share_link(descriptor: &NodeDescriptor) -> Result<String> {
    let bytes =
        to_cbor_vec(descriptor).map_err(|e| DiscoveryError::MalformedLink(e.to_string()))?;
    Ok(format!(
        "{SHARE_LINK_SCHEME}://{}",
        data_encoding::BASE64URL_NOPAD.encode(&bytes)
    ))
}

/// Decode and verify an `ourvideo://` link.
pub fn parse_share_link(link: &str) -> Result<NodeDescriptor> {
    let body = link
        .trim()
        .strip_prefix(&format!("{SHARE_LINK_SCHEME}://"))
        .ok_or_else(|| DiscoveryError::MalformedLink("missing ourvideo:// prefix".to_string()))?;
    parse_share_link_body(body)
}

fn parse_share_link_body(body: &str) -> Result<NodeDescriptor> {
    let body = body.trim_end_matches('/');
    // Cap the input before decoding so a huge paste cannot make us allocate.
    if body.len() > MAX_DESCRIPTOR_SIZE * 2 {
        return Err(DiscoveryError::MalformedLink(format!(
            "link is {} characters, the limit is {}",
            body.len(),
            MAX_DESCRIPTOR_SIZE * 2
        )));
    }
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(body.as_bytes())
        .map_err(|e| DiscoveryError::MalformedLink(e.to_string()))?;
    let descriptor: NodeDescriptor =
        from_cbor_slice(&bytes).map_err(|e| DiscoveryError::MalformedLink(e.to_string()))?;
    descriptor
        .verify()
        .map_err(|source| DiscoveryError::Untrusted {
            url: "share link".to_string(),
            source,
        })?;
    Ok(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_identity::Identity;
    use ovn_protocol::Capability;

    fn descriptor() -> (NodeDescriptor, Identity) {
        let identity = Identity::generate();
        let descriptor = NodeDescriptor::sign(
            "Example node".into(),
            vec!["/ip4/192.0.2.10/udp/4800/quic-v1".into()],
            vec![Capability::bootstrap()],
            &identity,
        )
        .unwrap();
        (descriptor, identity)
    }

    #[test]
    fn a_share_link_round_trips() {
        let (descriptor, _) = descriptor();
        let link = share_link(&descriptor).unwrap();
        assert!(link.starts_with("ourvideo://"));
        assert_eq!(parse_share_link(&link).unwrap(), descriptor);
    }

    #[test]
    fn a_tampered_share_link_is_refused() {
        let (mut descriptor, _) = descriptor();
        descriptor.node_name = "Impostor".into();
        let link = share_link(&descriptor).unwrap();
        assert!(matches!(
            parse_share_link(&link),
            Err(DiscoveryError::Untrusted { .. })
        ));
    }

    #[test]
    fn garbage_links_are_refused_without_panicking() {
        for link in [
            "ourvideo://",
            "ourvideo://!!!!",
            "ourvideo://aGVsbG8",
            "not a link",
        ] {
            assert!(parse_share_link(link).is_err(), "{link}");
        }
    }

    #[test]
    fn an_enormous_link_is_refused_before_decoding() {
        let link = format!("ourvideo://{}", "A".repeat(MAX_DESCRIPTOR_SIZE * 2 + 1));
        assert!(matches!(
            parse_share_link(&link),
            Err(DiscoveryError::MalformedLink(_))
        ));
    }

    #[test]
    fn urls_hostnames_links_and_addresses_are_told_apart() {
        let (descriptor, _) = descriptor();
        let link = share_link(&descriptor).unwrap();

        assert!(matches!(
            Target::parse(&link).unwrap(),
            Target::ShareLink(_)
        ));
        assert!(matches!(
            Target::parse("https://video.example.jp").unwrap(),
            Target::Url(u) if u == "https://video.example.jp"
        ));
        assert!(matches!(
            Target::parse("http://192.0.2.1:8080").unwrap(),
            Target::Url(_)
        ));
        // A bare hostname is the example given in the design.
        assert!(matches!(
            Target::parse("video.example.jp").unwrap(),
            Target::Url(u) if u == "https://video.example.jp"
        ));
        assert!(matches!(
            Target::parse("/ip4/192.0.2.1/udp/4800/quic-v1").unwrap(),
            Target::Address(_)
        ));
    }

    #[test]
    fn surrounding_whitespace_is_forgiven() {
        assert!(matches!(
            Target::parse("  https://video.example.jp  ").unwrap(),
            Target::Url(_)
        ));
    }

    #[test]
    fn hostile_schemes_are_refused() {
        for input in ["file:///etc/passwd", "ftp://example.com", "javascript://x"] {
            assert!(
                matches!(
                    Target::parse(input),
                    Err(DiscoveryError::UnsupportedScheme(_))
                ),
                "{input}"
            );
        }
    }

    #[test]
    fn nonsense_is_refused() {
        for input in ["", "   ", "hello world", "/not/a/multiaddr"] {
            assert!(Target::parse(input).is_err(), "{input:?}");
        }
    }
}
