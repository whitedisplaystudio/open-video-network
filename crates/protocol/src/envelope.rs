//! The signed envelope every direct peer-to-peer message travels in
//! (section 21), plus the request/response payloads carried inside it.

use ciborium::Value;
use ovn_identity::{Identity, PublicKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::codec::value_to_vec;
use crate::{
    check_not_future, check_version, from_cbor_slice, to_cbor_vec, ContentId, ProtocolError,
    Result, MAX_MESSAGE_SIZE, MAX_PROVIDERS_PER_RESPONSE, MAX_QUERY_LEN, MAX_QUERY_RESULTS,
    PROTOCOL_VERSION,
};

const ENVELOPE_DOMAIN: &str = "ovn/envelope/v1";

/// The kind of message an [`Envelope`] carries.
///
/// An unrecognised type deserialises into [`MessageType::Unknown`] rather than
/// failing, so a v1 node can sit on a network where v2 nodes are chatting
/// about things it does not understand.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MessageType {
    PeerHello,
    VideoAnnounce,
    VideoQuery,
    VideoProvider,
    ProfileUpdate,
    Follow,
    Unknown(String),
}

impl MessageType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::PeerHello => "PEER_HELLO",
            Self::VideoAnnounce => "VIDEO_ANNOUNCE",
            Self::VideoQuery => "VIDEO_QUERY",
            Self::VideoProvider => "VIDEO_PROVIDER",
            Self::ProfileUpdate => "PROFILE_UPDATE",
            Self::Follow => "FOLLOW",
            Self::Unknown(s) => s,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl From<&str> for MessageType {
    fn from(s: &str) -> Self {
        match s {
            "PEER_HELLO" => Self::PeerHello,
            "VIDEO_ANNOUNCE" => Self::VideoAnnounce,
            "VIDEO_QUERY" => Self::VideoQuery,
            "VIDEO_PROVIDER" => Self::VideoProvider,
            "PROFILE_UPDATE" => Self::ProfileUpdate,
            "FOLLOW" => Self::Follow,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for MessageType {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MessageType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Ok(MessageType::from(
            String::deserialize(deserializer)?.as_str(),
        ))
    }
}

/// The outer frame: who sent this, when, what kind of thing it is, and proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub protocol_version: u16,
    pub message_type: MessageType,
    /// Raw Ed25519 public key of the sender.
    #[serde(with = "serde_bytes")]
    pub sender: Vec<u8>,
    pub timestamp: u64,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl Envelope {
    /// Wrap and sign a payload.
    pub fn seal<T: Serialize>(
        message_type: MessageType,
        payload: &T,
        identity: &Identity,
    ) -> Result<Self> {
        let payload = to_cbor_vec(payload)?;
        let mut envelope = Self {
            protocol_version: PROTOCOL_VERSION,
            message_type,
            sender: identity.public_key().to_vec(),
            timestamp: crate::now_secs(),
            payload,
            signature: Vec::new(),
        };
        envelope.signature = identity.sign(&envelope.signing_bytes());
        Ok(envelope)
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text(ENVELOPE_DOMAIN.to_string()),
            Value::Integer(self.protocol_version.into()),
            Value::Text(self.message_type.as_str().to_string()),
            Value::Bytes(self.sender.clone()),
            Value::Integer(self.timestamp.into()),
            Value::Bytes(self.payload.clone()),
        ]);
        value_to_vec(&value)
    }

    /// Verify the envelope itself. The payload is still untrusted afterwards
    /// and must be validated by whoever decodes it.
    pub fn verify(&self) -> Result<()> {
        check_version(self.protocol_version)?;
        if self.payload.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge {
                actual: self.payload.len(),
                limit: MAX_MESSAGE_SIZE,
            });
        }
        check_not_future(self.timestamp)?;
        PublicKey::from_bytes(&self.sender).map_err(|_| ProtocolError::InvalidPublicKey)?;
        if self.signature.len() != ovn_identity::SIGNATURE_LEN {
            return Err(ProtocolError::InvalidSignatureLength(self.signature.len()));
        }
        if !ovn_identity::verify(&self.sender, &self.signing_bytes(), &self.signature) {
            return Err(ProtocolError::BadSignature);
        }
        Ok(())
    }

    /// Verify, then decode the payload.
    pub fn open<T: DeserializeOwned>(&self) -> Result<T> {
        self.verify()?;
        from_cbor_slice(&self.payload)
    }

    pub fn sender_key(&self) -> Result<PublicKey> {
        PublicKey::from_bytes(&self.sender).map_err(|_| ProtocolError::InvalidPublicKey)
    }
}

/// `VIDEO_QUERY`: ask a peer what it knows about a phrase.
///
/// This is a *content* search sent to a specific peer on request. It is not
/// the local search in section 26, which never leaves the device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoQuery {
    pub query: String,
    pub limit: u16,
}

impl VideoQuery {
    pub fn validate(&self) -> Result<()> {
        crate::check_len("query", self.query.len(), MAX_QUERY_LEN)?;
        if self.limit as usize > MAX_QUERY_RESULTS {
            return Err(ProtocolError::FieldTooLarge {
                field: "limit",
                actual: self.limit as usize,
                limit: MAX_QUERY_RESULTS,
            });
        }
        Ok(())
    }
}

/// `VIDEO_PROVIDER`: peers believed to hold a piece of content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoProvider {
    pub video_cid: ContentId,
    /// Base58 peer ids.
    pub providers: Vec<String>,
}

impl VideoProvider {
    pub fn validate(&self) -> Result<()> {
        crate::check_len(
            "providers",
            self.providers.len(),
            MAX_PROVIDERS_PER_RESPONSE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_envelope_opens() {
        let id = Identity::generate();
        let query = VideoQuery {
            query: "rust".into(),
            limit: 10,
        };
        let envelope = Envelope::seal(MessageType::VideoQuery, &query, &id).unwrap();
        assert_eq!(envelope.open::<VideoQuery>().unwrap(), query);
        assert_eq!(envelope.sender_key().unwrap(), id.public_key());
    }

    #[test]
    fn tampering_with_the_payload_is_detected() {
        let id = Identity::generate();
        let mut envelope = Envelope::seal(
            MessageType::PeerHello,
            &VideoQuery {
                query: "a".into(),
                limit: 1,
            },
            &id,
        )
        .unwrap();
        envelope.payload.push(0);
        assert_eq!(envelope.verify(), Err(ProtocolError::BadSignature));
    }

    #[test]
    fn changing_the_message_type_is_detected() {
        let id = Identity::generate();
        let mut envelope = Envelope::seal(MessageType::PeerHello, &(), &id).unwrap();
        envelope.message_type = MessageType::Follow;
        assert_eq!(envelope.verify(), Err(ProtocolError::BadSignature));
    }

    #[test]
    fn unknown_message_types_round_trip_instead_of_failing() {
        let raw = MessageType::from("SOMETHING_NEW");
        assert_eq!(raw, MessageType::Unknown("SOMETHING_NEW".into()));
        assert!(!raw.is_known());
        let bytes = to_cbor_vec(&raw).unwrap();
        let back: MessageType = from_cbor_slice(&bytes).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn known_message_types_round_trip() {
        for kind in [
            MessageType::PeerHello,
            MessageType::VideoAnnounce,
            MessageType::VideoQuery,
            MessageType::VideoProvider,
            MessageType::ProfileUpdate,
            MessageType::Follow,
        ] {
            let bytes = to_cbor_vec(&kind).unwrap();
            assert_eq!(from_cbor_slice::<MessageType>(&bytes).unwrap(), kind);
            assert!(kind.is_known());
        }
    }

    #[test]
    fn query_limits_are_enforced() {
        assert!(VideoQuery {
            query: "x".repeat(MAX_QUERY_LEN + 1),
            limit: 1
        }
        .validate()
        .is_err());
        assert!(VideoQuery {
            query: "x".into(),
            limit: (MAX_QUERY_RESULTS + 1) as u16
        }
        .validate()
        .is_err());
        assert!(VideoQuery {
            query: "x".into(),
            limit: 10
        }
        .validate()
        .is_ok());
    }
}
