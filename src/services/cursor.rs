use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::hash::{BuildHasher, Hash, Hasher};

use crate::{Error, Result};

const CURSOR_VERSION: u8 = 1;
const MAX_CURSOR_BYTES: usize = 2 * 1024;
const TAG_BYTES: usize = 16;

/// The single authenticated cursor codec for every retrieval projection.
#[derive(Debug, Clone)]
pub(super) struct CursorCodec {
    repository: String,
    key: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u8,
    repository: String,
    generation: u64,
    request_digest: String,
    position: T,
}

impl CursorCodec {
    pub(super) fn new(repository: String) -> Self {
        let random = std::collections::hash_map::RandomState::new();
        let mut key = [0_u8; 32];
        for (domain, chunk) in key.chunks_exact_mut(8).enumerate() {
            let mut hasher = random.build_hasher();
            "leantoken-cursor-auth-v1".hash(&mut hasher);
            repository.hash(&mut hasher);
            crate::config::INDEX_CONTENT_VERSION.hash(&mut hasher);
            domain.hash(&mut hasher);
            chunk.copy_from_slice(&hasher.finish().to_le_bytes());
        }
        Self { repository, key }
    }

    pub(super) fn seal<T: Serialize>(
        &self,
        generation: u64,
        request_digest: &str,
        position: &T,
    ) -> Result<String> {
        let payload = serde_json::to_vec(&Envelope {
            version: CURSOR_VERSION,
            repository: self.repository.clone(),
            generation,
            request_digest: request_digest.to_owned(),
            position,
        })?;
        let tag = blake3::keyed_hash(&self.key, &payload);
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(&tag.as_bytes()[..TAG_BYTES])
        ))
    }

    pub(super) fn open<T: DeserializeOwned>(
        &self,
        cursor: &str,
        generation: u64,
        request_digest: &str,
    ) -> Result<T> {
        if cursor.len() > MAX_CURSOR_BYTES {
            return Err(Error::StaleCursor);
        }
        let (payload, supplied_tag) = cursor.split_once('.').ok_or(Error::StaleCursor)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Error::StaleCursor)?;
        let supplied_tag = URL_SAFE_NO_PAD
            .decode(supplied_tag)
            .map_err(|_| Error::StaleCursor)?;
        let expected_tag = blake3::keyed_hash(&self.key, &payload);
        if supplied_tag.len() != TAG_BYTES
            || !constant_time_eq::constant_time_eq(
                &supplied_tag,
                &expected_tag.as_bytes()[..TAG_BYTES],
            )
        {
            return Err(Error::StaleCursor);
        }
        let envelope: Envelope<T> =
            serde_json::from_slice(&payload).map_err(|_| Error::StaleCursor)?;
        if envelope.version != CURSOR_VERSION
            || envelope.repository != self.repository
            || envelope.generation != generation
            || envelope.request_digest != request_digest
        {
            return Err(Error::StaleCursor);
        }
        Ok(envelope.position)
    }
}

pub(super) fn request_digest(kind: &str, normalized_request: &impl Serialize) -> Result<String> {
    let encoded = serde_json::to_vec(&(kind, normalized_request))?;
    Ok(blake3::hash(&encoded).to_hex()[..32].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_cursor_binds_repository_generation_request_and_position() {
        let codec = CursorCodec::new("repository-a".into());
        let cursor = codec.seal(7, "request-a", &42_u64).expect("seal");
        assert_eq!(codec.open::<u64>(&cursor, 7, "request-a").unwrap(), 42);
        assert!(matches!(
            codec.open::<u64>(&cursor, 8, "request-a"),
            Err(Error::StaleCursor)
        ));
        assert!(matches!(
            codec.open::<u64>(&cursor, 7, "request-b"),
            Err(Error::StaleCursor)
        ));
        assert!(matches!(
            CursorCodec::new("repository-b".into()).open::<u64>(&cursor, 7, "request-a"),
            Err(Error::StaleCursor)
        ));
    }

    #[test]
    fn sealed_cursor_rejects_tampering() {
        let codec = CursorCodec::new("repository-a".into());
        let mut cursor = codec.seal(7, "request", &42_u64).expect("seal");
        cursor.replace_range(0..1, if cursor.starts_with('A') { "B" } else { "A" });
        assert!(matches!(
            codec.open::<u64>(&cursor, 7, "request"),
            Err(Error::StaleCursor)
        ));
    }

    #[test]
    fn sealed_cursor_is_a_process_capability() {
        let first = CursorCodec::new("repository-a".into());
        let second = CursorCodec::new("repository-a".into());
        let cursor = first.seal(7, "request", &42_u64).expect("seal");
        assert!(matches!(
            second.open::<u64>(&cursor, 7, "request"),
            Err(Error::StaleCursor)
        ));
    }
}
