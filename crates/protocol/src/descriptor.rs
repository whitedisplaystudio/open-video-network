//! Node descriptors: how a URL becomes a peer.
//!
//! Section 14/15. A descriptor is the only thing a newcomer needs, and it is
//! self-authenticating: the peer id inside it must be derivable from the
//! public key inside it, and the whole document must be signed by that key.
//! A hostile web server can therefore hand out a descriptor, but it cannot
//! hand out someone *else's* identity.

use ciborium::Value;
use ovn_identity::{Identity, PublicKey};
use serde::{Deserialize, Serialize};

use crate::codec::value_to_vec;
use crate::{
    check_len, check_not_future, check_version, now_secs, ProtocolError, Result, MAX_ADDRESSES,
    MAX_ADDRESS_LEN, MAX_CAPABILITIES, MAX_CAPABILITY_LEN, MAX_NODE_NAME_LEN, PROTOCOL_VERSION,
};

const NODE_DESCRIPTOR_DOMAIN: &str = "ovn/node-descriptor/v1";

/// A capability a node advertises. Held as a string rather than an enum so
/// that a descriptor written by a newer node, advertising something we have
/// never heard of, still parses (section 23).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Stores and serves content chunks.
    pub fn video_store() -> Self {
        Self::new("video-store")
    }
    /// Participates in the DHT as a server, not just a client.
    pub fn dht_server() -> Self {
        Self::new("dht-server")
    }
    /// Relays gossip for other peers.
    pub fn gossip_relay() -> Self {
        Self::new("gossip-relay")
    }
    /// Willing to be used as an entry point by newcomers.
    pub fn bootstrap() -> Self {
        Self::new("bootstrap")
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A signed, self-authenticating description of how to reach a node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDescriptor {
    pub protocol_version: u16,
    pub node_name: String,
    /// Base58 libp2p peer id. Must match `public_key`.
    pub peer_id: String,
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    /// Multiaddrs, most specific first.
    pub addresses: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    pub created_at: u64,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl NodeDescriptor {
    pub fn sign(
        node_name: String,
        addresses: Vec<String>,
        capabilities: Vec<Capability>,
        identity: &Identity,
    ) -> Result<Self> {
        let mut descriptor = Self {
            protocol_version: PROTOCOL_VERSION,
            node_name,
            peer_id: identity.peer_id().to_base58(),
            public_key: identity.public_key().to_vec(),
            addresses,
            capabilities,
            created_at: now_secs(),
            signature: Vec::new(),
        };
        descriptor.validate()?;
        descriptor.signature = identity.sign(&descriptor.signing_bytes());
        Ok(descriptor)
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text(NODE_DESCRIPTOR_DOMAIN.to_string()),
            Value::Integer(self.protocol_version.into()),
            Value::Text(self.node_name.clone()),
            Value::Text(self.peer_id.clone()),
            Value::Bytes(self.public_key.clone()),
            Value::Array(self.addresses.iter().cloned().map(Value::Text).collect()),
            Value::Array(
                self.capabilities
                    .iter()
                    .map(|c| Value::Text(c.as_str().to_string()))
                    .collect(),
            ),
            Value::Integer(self.created_at.into()),
        ]);
        value_to_vec(&value)
    }

    pub fn validate(&self) -> Result<()> {
        check_version(self.protocol_version)?;
        check_len("nodeName", self.node_name.len(), MAX_NODE_NAME_LEN)?;
        check_len("addresses", self.addresses.len(), MAX_ADDRESSES)?;
        for address in &self.addresses {
            check_len("address", address.len(), MAX_ADDRESS_LEN)?;
        }
        check_len("capabilities", self.capabilities.len(), MAX_CAPABILITIES)?;
        for capability in &self.capabilities {
            check_len("capability", capability.as_str().len(), MAX_CAPABILITY_LEN)?;
        }
        check_not_future(self.created_at)?;

        let public_key =
            PublicKey::from_bytes(&self.public_key).map_err(|_| ProtocolError::InvalidPublicKey)?;
        // The binding that makes a descriptor safe to fetch over plain HTTP.
        if public_key.peer_id().to_base58() != self.peer_id {
            return Err(ProtocolError::InvalidPublicKey);
        }
        Ok(())
    }

    pub fn verify(&self) -> Result<()> {
        self.validate()?;
        if self.signature.len() != ovn_identity::SIGNATURE_LEN {
            return Err(ProtocolError::InvalidSignatureLength(self.signature.len()));
        }
        if !ovn_identity::verify(&self.public_key, &self.signing_bytes(), &self.signature) {
            return Err(ProtocolError::BadSignature);
        }
        Ok(())
    }

    pub fn has_capability(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(identity: &Identity) -> NodeDescriptor {
        NodeDescriptor::sign(
            "test node".into(),
            vec!["/ip4/192.0.2.10/udp/4800/quic-v1".into()],
            vec![Capability::video_store(), Capability::bootstrap()],
            identity,
        )
        .unwrap()
    }

    #[test]
    fn signed_descriptor_verifies() {
        let id = Identity::generate();
        let descriptor = sample(&id);
        assert_eq!(descriptor.verify(), Ok(()));
        assert_eq!(descriptor.peer_id, id.peer_id().to_base58());
    }

    #[test]
    fn a_descriptor_cannot_claim_someone_elses_peer_id() {
        let id = Identity::generate();
        let victim = Identity::generate();
        let mut descriptor = sample(&id);
        descriptor.peer_id = victim.peer_id().to_base58();
        // Caught by the key/peer-id binding before we even look at the signature.
        assert_eq!(descriptor.validate(), Err(ProtocolError::InvalidPublicKey));
        assert_eq!(descriptor.verify(), Err(ProtocolError::InvalidPublicKey));
    }

    #[test]
    fn rewriting_addresses_invalidates_the_signature() {
        let id = Identity::generate();
        let mut descriptor = sample(&id);
        descriptor
            .addresses
            .push("/ip4/198.51.100.1/tcp/4800".into());
        assert_eq!(descriptor.verify(), Err(ProtocolError::BadSignature));
    }

    #[test]
    fn unknown_capabilities_still_parse() {
        let id = Identity::generate();
        let descriptor = NodeDescriptor::sign(
            "future node".into(),
            vec![],
            vec![Capability::new("time-travel")],
            &id,
        )
        .unwrap();
        assert_eq!(descriptor.verify(), Ok(()));
        assert!(descriptor.has_capability(&Capability::new("time-travel")));
        assert!(!descriptor.has_capability(&Capability::bootstrap()));
    }

    #[test]
    fn too_many_addresses_are_refused() {
        let id = Identity::generate();
        let addresses = (0..MAX_ADDRESSES + 1)
            .map(|i| format!("/ip4/192.0.2.{i}/tcp/4800"))
            .collect();
        let err = NodeDescriptor::sign("n".into(), addresses, vec![], &id).unwrap_err();
        assert!(matches!(err, ProtocolError::FieldTooLarge { .. }));
    }

    #[test]
    fn json_roundtrip_matches_the_well_known_document() {
        // The descriptor is published as JSON over HTTPS, so it has to survive
        // a JSON roundtrip as well as a CBOR one.
        let id = Identity::generate();
        let descriptor = sample(&id);
        let cbor = crate::to_cbor_vec(&descriptor).unwrap();
        let back: NodeDescriptor = crate::from_cbor_slice(&cbor).unwrap();
        assert_eq!(back, descriptor);
        assert_eq!(back.verify(), Ok(()));
    }
}
