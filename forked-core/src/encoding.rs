//! Deterministic object encoding.
//!
//! Every stored object is CBOR produced by this module. The exact byte
//! layout is specified in `docs/FORMAT.md`; the invariant that matters is
//! that encoding is a pure function of the logical value, the same node or
//! commit always encodes to the same bytes, so content hashes are stable.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{Error, Hash, Result, Storage};

/// Encode a value to deterministic CBOR bytes.
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(value, &mut buf).map_err(|e| Error::Encoding(e.to_string()))?;
    Ok(buf)
}

/// Decode CBOR bytes back into a value.
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ciborium::de::from_reader(bytes).map_err(|e| Error::Corrupt(e.to_string()))
}

/// Encode `value`, store it as a blob, and return its content hash.
pub(crate) async fn put_object<S: Storage, T: Serialize>(storage: &S, value: &T) -> Result<Hash> {
    let bytes = encode(value)?;
    let hash = Hash::of(&bytes);
    storage.put_blob(&hash, &bytes).await?;
    Ok(hash)
}

/// Fetch and decode the blob at `hash`, failing if it is absent.
pub(crate) async fn get_object<S: Storage, T: DeserializeOwned>(
    storage: &S,
    hash: &Hash,
) -> Result<T> {
    let bytes = storage
        .get_blob(hash)
        .await?
        .ok_or(Error::MissingBlob(*hash))?;
    decode(&bytes)
}
