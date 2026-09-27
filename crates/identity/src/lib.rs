//! Identity primitives for the Open Video Network.
//!
//! A node's identity is a single Ed25519 key pair generated locally on first
//! start. There is no account server: the public key *is* the identity, and the
//! libp2p `PeerId` is derived from it so that the network layer and the
//! application layer can never disagree about who a peer is.
//!
//! The secret key never leaves the machine. Nothing in this crate serialises a
//! secret key into a network-facing type.

use std::fmt;
use std::fs;
use std::path::Path;

use libp2p_identity::ed25519;
pub use libp2p_identity::{Keypair as Libp2pKeypair, PeerId};

/// Length of a raw Ed25519 public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length of a raw Ed25519 secret key seed.
pub const SECRET_KEY_LEN: usize = 32;
/// Length of an Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity key file is malformed: expected {SECRET_KEY_LEN} bytes, found {0}")]
    MalformedKeyFile(usize),
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid key material: {0}")]
    InvalidKeyMaterial(String),
    #[error("i/o error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, IdentityError>;

/// A raw Ed25519 public key. This is the user-visible identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; PUBLIC_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| IdentityError::InvalidPublicKey)?;
        // Reject anything that is not a valid curve point.
        ed25519::PublicKey::try_from_bytes(&arr).map_err(|_| IdentityError::InvalidPublicKey)?;
        Ok(Self(arr))
    }

    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    /// Lowercase hex, used wherever a public key needs a stable text form
    /// (database keys, CLI output, log lines).
    pub fn to_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let raw = data_encoding::HEXLOWER_PERMISSIVE
            .decode(s.as_bytes())
            .map_err(|_| IdentityError::InvalidPublicKey)?;
        Self::from_bytes(&raw)
    }

    /// The libp2p `PeerId` this identity presents on the wire.
    pub fn peer_id(&self) -> PeerId {
        self.to_libp2p().to_peer_id()
    }

    fn to_libp2p(self) -> libp2p_identity::PublicKey {
        let pk = ed25519::PublicKey::try_from_bytes(&self.0)
            .expect("public key validated on construction");
        pk.into()
    }

    /// Verify a detached Ed25519 signature over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        if signature.len() != SIGNATURE_LEN {
            return false;
        }
        let pk = match ed25519::PublicKey::try_from_bytes(&self.0) {
            Ok(pk) => pk,
            Err(_) => return false,
        };
        pk.verify(message, signature)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Verify a signature against a raw public key without constructing a
/// [`PublicKey`] first. Returns `false` for any malformed input rather than an
/// error: every caller is handling untrusted network data.
pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    match PublicKey::from_bytes(public_key) {
        Ok(pk) => pk.verify(message, signature),
        Err(_) => false,
    }
}

/// Derive the libp2p `PeerId` for a raw Ed25519 public key.
pub fn peer_id_from_public_key(public_key: &[u8]) -> Result<PeerId> {
    Ok(PublicKey::from_bytes(public_key)?.peer_id())
}

/// A locally held Ed25519 key pair.
///
/// `Identity` deliberately implements neither `Serialize` nor `Debug` printing
/// of secret material: the only way the secret leaves the process is
/// [`Identity::save`], which writes it to a `0600` file on disk.
#[derive(Clone)]
pub struct Identity {
    keypair: ed25519::Keypair,
}

impl Identity {
    /// Generate a fresh identity.
    pub fn generate() -> Self {
        Self {
            keypair: ed25519::Keypair::generate(),
        }
    }

    /// Rebuild an identity from its 32-byte secret seed.
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self> {
        let mut seed: [u8; SECRET_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| IdentityError::MalformedKeyFile(bytes.len()))?;
        let secret = ed25519::SecretKey::try_from_bytes(&mut seed)
            .map_err(|e| IdentityError::InvalidKeyMaterial(e.to_string()))?;
        Ok(Self {
            keypair: secret.into(),
        })
    }

    /// Load the identity at `path`, generating and persisting one if the file
    /// does not exist yet. This is what makes `ourvideo start` a single
    /// command: the user is never asked to create or manage a key.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let bytes = fs::read(path).map_err(|source| IdentityError::Io {
                path: path.display().to_string(),
                source,
            })?;
            return Self::from_secret_bytes(&bytes);
        }
        let identity = Self::generate();
        identity.save(path)?;
        Ok(identity)
    }

    /// Write the secret seed to `path` with owner-only permissions.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| IdentityError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let io_err = |source| IdentityError::Io {
            path: path.display().to_string(),
            source,
        };
        fs::write(path, self.secret_bytes()).map_err(io_err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_err)?;
        }
        Ok(())
    }

    /// The 32-byte secret seed. Only [`Identity::save`] should ever call this.
    fn secret_bytes(&self) -> [u8; SECRET_KEY_LEN] {
        let mut out = [0u8; SECRET_KEY_LEN];
        out.copy_from_slice(self.keypair.secret().as_ref());
        out
    }

    pub fn public_key(&self) -> PublicKey {
        let mut out = [0u8; PUBLIC_KEY_LEN];
        out.copy_from_slice(&self.keypair.public().to_bytes());
        PublicKey(out)
    }

    pub fn peer_id(&self) -> PeerId {
        self.public_key().peer_id()
    }

    /// Sign `message`, producing a detached 64-byte Ed25519 signature.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.keypair.sign(message)
    }

    /// The key pair in the form the libp2p swarm expects.
    pub fn libp2p_keypair(&self) -> Libp2pKeypair {
        Libp2pKeypair::from(self.keypair.clone())
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never render secret material, not even behind `{:?}` in a log line.
        f.debug_struct("Identity")
            .field("public_key", &self.public_key().to_hex())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let id = Identity::generate();
        let sig = id.sign(b"hello");
        assert!(id.public_key().verify(b"hello", &sig));
        assert!(verify(id.public_key().as_bytes(), b"hello", &sig));
    }

    #[test]
    fn tampered_message_fails_verification() {
        let id = Identity::generate();
        let sig = id.sign(b"hello");
        assert!(!id.public_key().verify(b"hello!", &sig));
    }

    #[test]
    fn signature_from_another_key_fails() {
        let a = Identity::generate();
        let b = Identity::generate();
        let sig = b.sign(b"hello");
        assert!(!a.public_key().verify(b"hello", &sig));
    }

    #[test]
    fn malformed_signature_is_rejected_not_panicking() {
        let id = Identity::generate();
        assert!(!id.public_key().verify(b"hello", &[]));
        assert!(!id.public_key().verify(b"hello", &[0u8; 8]));
        assert!(!verify(&[0u8; 3], b"hello", &[0u8; SIGNATURE_LEN]));
    }

    #[test]
    fn peer_id_is_derived_from_public_key() {
        let id = Identity::generate();
        let from_identity = id.peer_id();
        let from_bytes = peer_id_from_public_key(id.public_key().as_bytes()).unwrap();
        let from_libp2p = id.libp2p_keypair().public().to_peer_id();
        assert_eq!(from_identity, from_bytes);
        assert_eq!(from_identity, from_libp2p);
    }

    #[test]
    fn public_key_hex_roundtrip() {
        let id = Identity::generate();
        let pk = id.public_key();
        assert_eq!(PublicKey::from_hex(&pk.to_hex()).unwrap(), pk);
    }

    #[test]
    fn load_or_create_is_stable_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        let first = Identity::load_or_create(&path).unwrap();
        let second = Identity::load_or_create(&path).unwrap();
        assert_eq!(first.public_key(), second.public_key());
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        Identity::load_or_create(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn malformed_key_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        fs::write(&path, b"too short").unwrap();
        assert!(matches!(
            Identity::load_or_create(&path),
            Err(IdentityError::MalformedKeyFile(9))
        ));
    }

    #[test]
    fn debug_does_not_leak_secret() {
        let id = Identity::generate();
        let rendered = format!("{id:?}");
        let secret_hex = data_encoding::HEXLOWER.encode(&id.secret_bytes());
        assert!(!rendered.contains(&secret_hex));
    }
}
