//! CBOR encoding helpers with the size limits from section 31 applied.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{ProtocolError, Result, MAX_MESSAGE_SIZE};

/// Serialise a value to CBOR.
pub fn to_cbor_vec<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf)
        .map_err(|e| ProtocolError::MalformedCbor(e.to_string()))?;
    if buf.len() > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            actual: buf.len(),
            limit: MAX_MESSAGE_SIZE,
        });
    }
    Ok(buf)
}

/// Deserialise CBOR that arrived from an untrusted source.
///
/// The length check happens before parsing so that a malicious peer cannot
/// make us allocate by declaring a huge container.
pub fn from_cbor_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    from_cbor_slice_limited(bytes, MAX_MESSAGE_SIZE)
}

pub fn from_cbor_slice_limited<T: DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T> {
    if bytes.len() > limit {
        return Err(ProtocolError::MessageTooLarge {
            actual: bytes.len(),
            limit,
        });
    }
    ciborium::from_reader(bytes).map_err(|e| ProtocolError::MalformedCbor(e.to_string()))
}

/// Encode a `ciborium::Value` that was built by hand for signing.
pub(crate) fn value_to_vec(value: &ciborium::Value) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).expect("in-memory CBOR encoding cannot fail");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_input_is_rejected_before_parsing() {
        let big = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let err = from_cbor_slice::<u8>(&big).unwrap_err();
        assert!(matches!(err, ProtocolError::MessageTooLarge { .. }));
    }

    #[test]
    fn malformed_cbor_is_an_error_not_a_panic() {
        let err = from_cbor_slice::<String>(&[0xff, 0xff, 0xff]).unwrap_err();
        assert!(matches!(err, ProtocolError::MalformedCbor(_)));
    }

    #[test]
    fn roundtrip() {
        let encoded = to_cbor_vec(&vec!["a".to_string(), "b".to_string()]).unwrap();
        let decoded: Vec<String> = from_cbor_slice(&encoded).unwrap();
        assert_eq!(decoded, vec!["a".to_string(), "b".to_string()]);
    }
}
