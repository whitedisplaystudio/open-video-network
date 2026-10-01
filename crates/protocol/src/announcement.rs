//! Announcements: the light metadata that travels over GossipSub.
//!
//! Section 16: an announcement never carries video data, only enough
//! information for another node to decide whether it wants the content and
//! where to look for it.

use ciborium::Value;
use ovn_identity::{Identity, PublicKey};
use serde::{Deserialize, Serialize};

use crate::codec::value_to_vec;
use crate::{
    check_len, check_not_future, check_source_url, check_version, normalize_tag, now_secs,
    ContentId, ProtocolError, Result, MAX_BIO_LEN, MAX_DESCRIPTION_LEN, MAX_DISPLAY_NAME_LEN,
    MAX_DURATION_SECS, MAX_TAGS, MAX_TAG_LEN, MAX_TITLE_LEN, PROTOCOL_VERSION,
};

/// Domain separation tag mixed into the signed bytes so a signature over an
/// announcement can never be replayed as a signature over anything else.
const VIDEO_ANNOUNCE_DOMAIN: &str = "ovn/video-announce/v1";
const PROFILE_UPDATE_DOMAIN: &str = "ovn/profile-update/v1";

/// The fields a publisher supplies. Everything else — version, timestamp,
/// signature — is filled in by [`VideoAnnouncement::sign`].
#[derive(Clone, Debug, Default)]
pub struct NewVideo {
    pub video_cid: Option<ContentId>,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub duration_secs: u64,
    pub thumbnail_cid: Option<ContentId>,
    /// Where the video file itself can be fetched.
    pub source_url: String,
}

/// A signed claim by a creator that a video exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoAnnouncement {
    pub version: u16,
    pub video_cid: ContentId,
    #[serde(with = "serde_bytes")]
    pub creator_public_key: Vec<u8>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub duration_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_cid: Option<ContentId>,
    /// Where the video file is served from, as `https://…`.
    ///
    /// The network carries what is needed to find and judge a video — title,
    /// tags, thumbnail, and the manifest that says what the bytes must hash
    /// to. It does not carry the bytes. They come from here.
    ///
    /// Inside the signature, so whoever relays an announcement cannot point
    /// viewers somewhere the creator did not choose. What it cannot do is
    /// make the bytes trustworthy by itself — that is what `videoCid` is
    /// for.
    pub source_url: String,
    pub created_at: u64,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl VideoAnnouncement {
    /// Build and sign an announcement. Tags are normalised and de-duplicated
    /// first so that what gets signed is what every node will index.
    pub fn sign(new: NewVideo, identity: &Identity) -> Result<Self> {
        let video_cid = new.video_cid.ok_or(ProtocolError::FieldEmpty("videoCid"))?;
        let mut tags: Vec<String> = Vec::new();
        for tag in new.tags {
            let normalized = normalize_tag(&tag);
            if !normalized.is_empty() && !tags.contains(&normalized) {
                tags.push(normalized);
            }
        }
        let mut announcement = Self {
            version: PROTOCOL_VERSION,
            video_cid,
            creator_public_key: identity.public_key().to_vec(),
            title: new.title,
            description: new.description,
            tags,
            duration_secs: new.duration_secs,
            thumbnail_cid: new.thumbnail_cid,
            source_url: new.source_url,
            created_at: now_secs(),
            signature: Vec::new(),
        };
        announcement.validate()?;
        announcement.signature = identity.sign(&announcement.signing_bytes());
        Ok(announcement)
    }

    /// The canonical bytes a signature is computed over: a definite-length
    /// CBOR array, domain tag first, fields in the order given in
    /// `protocol/SPECIFICATION.md`. The signature field itself is excluded.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text(VIDEO_ANNOUNCE_DOMAIN.to_string()),
            Value::Integer(self.version.into()),
            Value::Text(self.video_cid.to_string()),
            Value::Bytes(self.creator_public_key.clone()),
            Value::Text(self.title.clone()),
            Value::Text(self.description.clone()),
            Value::Array(self.tags.iter().cloned().map(Value::Text).collect()),
            Value::Integer(self.duration_secs.into()),
            match &self.thumbnail_cid {
                Some(cid) => Value::Text(cid.to_string()),
                None => Value::Null,
            },
            Value::Text(self.source_url.clone()),
            Value::Integer(self.created_at.into()),
        ]);
        value_to_vec(&value)
    }

    /// Structural validation: everything checkable without cryptography.
    pub fn validate(&self) -> Result<()> {
        check_version(self.version)?;
        if self.title.trim().is_empty() {
            return Err(ProtocolError::FieldEmpty("title"));
        }
        check_len("title", self.title.len(), MAX_TITLE_LEN)?;
        check_len("description", self.description.len(), MAX_DESCRIPTION_LEN)?;
        check_len("tags", self.tags.len(), MAX_TAGS)?;
        for tag in &self.tags {
            check_len("tag", tag.len(), MAX_TAG_LEN)?;
        }
        if self.duration_secs > MAX_DURATION_SECS {
            return Err(ProtocolError::FieldTooLarge {
                field: "durationSecs",
                actual: self.duration_secs as usize,
                limit: MAX_DURATION_SECS as usize,
            });
        }
        if self.video_cid.is_manifest() {
            // A video id must address a manifest, not a bare chunk.
        } else {
            return Err(ProtocolError::InvalidContentId(
                "videoCid must address a dag-cbor manifest".to_string(),
            ));
        }
        check_source_url(&self.source_url)?;
        check_not_future(self.created_at)?;
        PublicKey::from_bytes(&self.creator_public_key)
            .map_err(|_| ProtocolError::InvalidPublicKey)?;
        Ok(())
    }

    /// Full check for an announcement received from the network: structure,
    /// then signature. Section 18: invalid signatures are discarded.
    pub fn verify(&self) -> Result<()> {
        self.validate()?;
        if self.signature.len() != ovn_identity::SIGNATURE_LEN {
            return Err(ProtocolError::InvalidSignatureLength(self.signature.len()));
        }
        if !ovn_identity::verify(
            &self.creator_public_key,
            &self.signing_bytes(),
            &self.signature,
        ) {
            return Err(ProtocolError::BadSignature);
        }
        Ok(())
    }

    pub fn creator(&self) -> Result<PublicKey> {
        PublicKey::from_bytes(&self.creator_public_key).map_err(|_| ProtocolError::InvalidPublicKey)
    }
}

/// A signed creator profile. Carried on its own GossipSub topic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdate {
    pub version: u16,
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    pub display_name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_cid: Option<ContentId>,
    pub updated_at: u64,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl ProfileUpdate {
    pub fn sign(display_name: String, bio: String, identity: &Identity) -> Result<Self> {
        let mut profile = Self {
            version: PROTOCOL_VERSION,
            public_key: identity.public_key().to_vec(),
            display_name,
            bio,
            avatar_cid: None,
            updated_at: now_secs(),
            signature: Vec::new(),
        };
        profile.validate()?;
        profile.signature = identity.sign(&profile.signing_bytes());
        Ok(profile)
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let value = Value::Array(vec![
            Value::Text(PROFILE_UPDATE_DOMAIN.to_string()),
            Value::Integer(self.version.into()),
            Value::Bytes(self.public_key.clone()),
            Value::Text(self.display_name.clone()),
            Value::Text(self.bio.clone()),
            match &self.avatar_cid {
                Some(cid) => Value::Text(cid.to_string()),
                None => Value::Null,
            },
            Value::Integer(self.updated_at.into()),
        ]);
        value_to_vec(&value)
    }

    pub fn validate(&self) -> Result<()> {
        check_version(self.version)?;
        if self.display_name.trim().is_empty() {
            return Err(ProtocolError::FieldEmpty("displayName"));
        }
        check_len("displayName", self.display_name.len(), MAX_DISPLAY_NAME_LEN)?;
        check_len("bio", self.bio.len(), MAX_BIO_LEN)?;
        check_not_future(self.updated_at)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{from_cbor_slice, to_cbor_vec};

    fn sample(identity: &Identity) -> VideoAnnouncement {
        VideoAnnouncement::sign(
            NewVideo {
                video_cid: Some(ContentId::from_dag_cbor(b"manifest bytes")),
                title: "Speedrun".to_string(),
                description: "A run".to_string(),
                tags: vec!["Gaming".into(), "gaming".into(), " Indie ".into()],
                duration_secs: 600,
                thumbnail_cid: None,
                source_url: "https://videos.example/clip.mp4".to_string(),
            },
            identity,
        )
        .unwrap()
    }

    #[test]
    fn signed_announcement_verifies() {
        let id = Identity::generate();
        let ann = sample(&id);
        assert_eq!(ann.verify(), Ok(()));
        assert_eq!(ann.creator().unwrap(), id.public_key());
    }

    #[test]
    fn tags_are_normalised_and_deduplicated_before_signing() {
        let id = Identity::generate();
        let ann = sample(&id);
        assert_eq!(ann.tags, vec!["gaming".to_string(), "indie".to_string()]);
    }

    #[test]
    fn tampering_with_the_title_invalidates_the_signature() {
        let id = Identity::generate();
        let mut ann = sample(&id);
        ann.title = "Something else".to_string();
        assert_eq!(ann.verify(), Err(ProtocolError::BadSignature));
    }

    #[test]
    fn tampering_with_tags_invalidates_the_signature() {
        let id = Identity::generate();
        let mut ann = sample(&id);
        ann.tags.push("music".to_string());
        assert_eq!(ann.verify(), Err(ProtocolError::BadSignature));
    }

    #[test]
    fn swapping_in_another_creator_key_invalidates_the_signature() {
        let id = Identity::generate();
        let other = Identity::generate();
        let mut ann = sample(&id);
        ann.creator_public_key = other.public_key().to_vec();
        assert_eq!(ann.verify(), Err(ProtocolError::BadSignature));
    }

    #[test]
    fn a_signature_over_a_profile_cannot_be_replayed_onto_an_announcement() {
        // Domain separation: the two message types never share signed bytes.
        let id = Identity::generate();
        let ann = sample(&id);
        let profile = ProfileUpdate::sign("n".into(), "b".into(), &id).unwrap();
        assert_ne!(ann.signing_bytes(), profile.signing_bytes());
    }

    #[test]
    fn empty_title_is_refused() {
        let id = Identity::generate();
        let err = VideoAnnouncement::sign(
            NewVideo {
                video_cid: Some(ContentId::from_dag_cbor(b"m")),
                title: "   ".to_string(),
                ..Default::default()
            },
            &id,
        )
        .unwrap_err();
        assert_eq!(err, ProtocolError::FieldEmpty("title"));
    }

    #[test]
    fn oversized_title_is_refused() {
        let id = Identity::generate();
        let err = VideoAnnouncement::sign(
            NewVideo {
                video_cid: Some(ContentId::from_dag_cbor(b"m")),
                title: "x".repeat(MAX_TITLE_LEN + 1),
                ..Default::default()
            },
            &id,
        )
        .unwrap_err();
        assert!(matches!(err, ProtocolError::FieldTooLarge { .. }));
    }

    #[test]
    fn a_chunk_cid_is_not_a_video_id() {
        let id = Identity::generate();
        let err = VideoAnnouncement::sign(
            NewVideo {
                video_cid: Some(ContentId::from_raw(b"a chunk")),
                title: "t".into(),
                ..Default::default()
            },
            &id,
        )
        .unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidContentId(_)));
    }

    #[test]
    fn future_timestamps_are_refused() {
        let id = Identity::generate();
        let mut ann = sample(&id);
        ann.created_at = now_secs() + 10_000;
        assert!(matches!(
            ann.validate(),
            Err(ProtocolError::TimestampInFuture { .. })
        ));
    }

    #[test]
    fn survives_a_cbor_roundtrip_with_the_signature_intact() {
        let id = Identity::generate();
        let ann = sample(&id);
        let bytes = to_cbor_vec(&ann).unwrap();
        let back: VideoAnnouncement = from_cbor_slice(&bytes).unwrap();
        assert_eq!(back, ann);
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn unknown_fields_from_a_future_version_are_ignored() {
        // Section 23: an optional field a newer node adds must not break us.
        let id = Identity::generate();
        let ann = sample(&id);
        let mut map: ciborium::Value = ciborium::from_reader(&to_cbor_vec(&ann).unwrap()[..])
            .expect("announcement encodes as a CBOR map");
        if let ciborium::Value::Map(entries) = &mut map {
            entries.push((
                ciborium::Value::Text("someFutureField".into()),
                ciborium::Value::Integer(7.into()),
            ));
        } else {
            panic!("expected a map");
        }
        let mut buf = Vec::new();
        ciborium::into_writer(&map, &mut buf).unwrap();
        let back: VideoAnnouncement = from_cbor_slice(&buf).unwrap();
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn profile_update_verifies_and_rejects_tampering() {
        let id = Identity::generate();
        let mut profile = ProfileUpdate::sign("Creator".into(), "bio".into(), &id).unwrap();
        assert_eq!(profile.verify(), Ok(()));
        profile.display_name = "Impostor".into();
        assert_eq!(profile.verify(), Err(ProtocolError::BadSignature));
    }
}
