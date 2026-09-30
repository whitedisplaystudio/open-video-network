//! Channels: subscribing to a creator rather than to a node.
//!
//! A node share link answers "how do I reach this machine". A channel link
//! answers "how do I keep seeing what this person publishes", which is a
//! different question and outlives any particular machine.
//!
//! The identity of a channel is a public key, not an address and not a name.
//! Every [`VideoAnnouncement`](crate::VideoAnnouncement) is signed by that
//! key, so a peer asked for a creator's announcements cannot invent them,
//! cannot alter them, and cannot leave out the fact that it is relaying
//! someone else's work. That is what lets a subscriber ask *anybody* rather
//! than having to reach the creator, and it is why a subscription keeps
//! working while the creator's own machine is switched off.

use ciborium::Value;
use ovn_identity::{Identity, PublicKey};
use serde::{Deserialize, Serialize};

use crate::codec::value_to_vec;
use crate::{
    check_len, check_not_future, check_version, now_secs, ProtocolError, Result, VideoAnnouncement,
    MAX_ADDRESSES, MAX_ADDRESS_LEN, MAX_DISPLAY_NAME_LEN, PROTOCOL_VERSION,
};

const CHANNEL_LINK_DOMAIN: &str = "ovn/channel-link/v1";

/// The most announcements one channel request may return.
///
/// A subscriber asks repeatedly with a later `since` rather than asking for
/// everything at once, so this bounds a single answer and not the archive.
pub const MAX_CHANNEL_ANNOUNCEMENTS: usize = 64;

/// What you hand somebody so they can subscribe to you.
///
/// Signed by the channel's own key, so the name inside it cannot be attached
/// to somebody else's identity by whoever passes the link along.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelLink {
    pub protocol_version: u16,
    /// The channel. Everything else here is a hint; this is the identity.
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    pub display_name: String,
    /// Where to start asking. Hints only: a subscription survives all of
    /// these going stale, because the DHT can be asked instead.
    #[serde(default)]
    pub addresses: Vec<String>,
    pub created_at: u64,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl ChannelLink {
    pub fn sign(display_name: String, addresses: Vec<String>, identity: &Identity) -> Result<Self> {
        let mut link = Self {
            protocol_version: PROTOCOL_VERSION,
            public_key: identity.public_key().to_vec(),
            display_name,
            addresses,
            created_at: now_secs(),
            signature: Vec::new(),
        };
        link.validate()?;
        link.signature = identity.sign(&link.signing_bytes());
        Ok(link)
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text(CHANNEL_LINK_DOMAIN.to_string()),
            Value::Integer(self.protocol_version.into()),
            Value::Bytes(self.public_key.clone()),
            Value::Text(self.display_name.clone()),
            Value::Array(self.addresses.iter().cloned().map(Value::Text).collect()),
            Value::Integer(self.created_at.into()),
        ]);
        value_to_vec(&value)
    }

    pub fn validate(&self) -> Result<()> {
        check_version(self.protocol_version)?;
        check_len("displayName", self.display_name.len(), MAX_DISPLAY_NAME_LEN)?;
        check_len("addresses", self.addresses.len(), MAX_ADDRESSES)?;
        for address in &self.addresses {
            check_len("address", address.len(), MAX_ADDRESS_LEN)?;
        }
        check_not_future(self.created_at)?;
        PublicKey::from_bytes(&self.public_key).map_err(|_| ProtocolError::InvalidPublicKey)?;
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

    pub fn creator(&self) -> Result<PublicKey> {
        PublicKey::from_bytes(&self.public_key).map_err(|_| ProtocolError::InvalidPublicKey)
    }
}

/// "What has this creator published?"
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRequest {
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    /// Only announcements published after this, so a subscriber that has
    /// already caught up asks for very little.
    #[serde(default)]
    pub since: u64,
}

/// Whatever the answering node happens to hold for that creator.
///
/// Every announcement carries the creator's own signature, so this is not a
/// statement the answering node is trusted for. It is free to send fewer than
/// it has, and it cannot send anything the creator did not sign.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelResponse {
    pub announcements: Vec<VideoAnnouncement>,
}

/// The DHT key under which nodes advertise that they can answer for a
/// creator.
///
/// Derived from the public key so that anybody holding the channel link can
/// compute it without asking anyone.
pub fn channel_provider_key(public_key: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(CHANNEL_LINK_DOMAIN.len() + 1 + public_key.len());
    key.extend_from_slice(CHANNEL_LINK_DOMAIN.as_bytes());
    key.push(b':');
    key.extend_from_slice(public_key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(identity: &Identity) -> ChannelLink {
        ChannelLink::sign(
            "Studio A".into(),
            vec!["/ip4/192.0.2.10/udp/4800/quic-v1".into()],
            identity,
        )
        .unwrap()
    }

    #[test]
    fn a_signed_link_verifies() {
        let identity = Identity::generate();
        assert!(link(&identity).verify().is_ok());
    }

    #[test]
    fn a_renamed_channel_does_not_verify() {
        // The point of signing the name: whoever passes the link on cannot
        // attach a different person's name to this key.
        let identity = Identity::generate();
        let mut tampered = link(&identity);
        tampered.display_name = "Someone Else".into();
        assert!(matches!(
            tampered.verify(),
            Err(ProtocolError::BadSignature)
        ));
    }

    #[test]
    fn addresses_cannot_be_added_by_a_relayer() {
        // Otherwise passing a link along would be a way to point subscribers
        // at a machine of your choosing.
        let identity = Identity::generate();
        let mut tampered = link(&identity);
        tampered
            .addresses
            .push("/ip4/198.51.100.1/udp/4800/quic-v1".into());
        assert!(matches!(
            tampered.verify(),
            Err(ProtocolError::BadSignature)
        ));
    }

    #[test]
    fn a_link_signed_by_one_key_cannot_claim_another() {
        let mine = Identity::generate();
        let theirs = Identity::generate();
        let mut forged = link(&mine);
        forged.public_key = theirs.public_key().to_vec();
        assert!(forged.verify().is_err());
    }

    #[test]
    fn provider_keys_follow_the_creator_and_nothing_else() {
        let one = Identity::generate();
        let two = Identity::generate();
        let a = one.public_key().to_vec();
        let b = two.public_key().to_vec();
        assert_eq!(channel_provider_key(&a), channel_provider_key(&a));
        assert_ne!(channel_provider_key(&a), channel_provider_key(&b));
        // And it must not collide with a raw public key used as a key
        // somewhere else.
        assert_ne!(channel_provider_key(&a), a);
    }

    #[test]
    fn a_link_from_the_future_is_rejected() {
        let identity = Identity::generate();
        let mut link = link(&identity);
        link.created_at = now_secs() + 86_400;
        link.signature = identity.sign(&link.signing_bytes());
        assert!(link.verify().is_err());
    }
}
