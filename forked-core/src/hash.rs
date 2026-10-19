//! Content-address hashes. Every stored object is addressed by the SHA-256
//! of its encoded bytes.

use core::fmt;

use serde::de::{Error as DeError, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// A 32-byte SHA-256 content hash.
///
/// Serialized as a CBOR byte string (major type 2), displayed as lowercase hex.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Hash the given bytes with SHA-256.
    pub fn of(bytes: &[u8]) -> Self {
        Hash(Sha256::digest(bytes).into())
    }

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex representation (64 characters).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-character hex string.
    pub fn from_hex(s: &str) -> Result<Self, crate::Error> {
        let bytes = hex::decode(s).map_err(|_| crate::Error::InvalidHash(s.to_string()))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| crate::Error::InvalidHash(s.to_string()))?;
        Ok(Hash(arr))
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", &self.to_hex()[..12])
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

struct HashVisitor;

impl<'de> Visitor<'de> for HashVisitor {
    type Value = Hash;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a 32-byte hash")
    }

    fn visit_bytes<E: DeError>(self, v: &[u8]) -> Result<Hash, E> {
        let arr: [u8; 32] = v
            .try_into()
            .map_err(|_| E::invalid_length(v.len(), &self))?;
        Ok(Hash(arr))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Hash, A::Error> {
        let mut arr = [0u8; 32];
        for (i, slot) in arr.iter_mut().enumerate() {
            *slot = seq
                .next_element()?
                .ok_or_else(|| A::Error::invalid_length(i, &self))?;
        }
        if seq.next_element::<u8>()?.is_some() {
            return Err(A::Error::invalid_length(33, &self));
        }
        Ok(Hash(arr))
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Hash, D::Error> {
        deserializer.deserialize_bytes(HashVisitor)
    }
}
