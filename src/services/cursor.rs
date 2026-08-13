//! Versioned integrity-checked cursors shared by immutable retrieval operations.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Serialize, de::DeserializeOwned};

use crate::services::index_read::RepositoryGeneration;
use crate::{Error, Result};

const CURSOR_VERSION: u8 = 2;

pub(crate) fn request_digest(request: &impl Serialize) -> Result<String> {
    let encoded = serde_json::to_vec(request)?;
    Ok(compact_hash(blake3::hash(&encoded), 16))
}

impl RepositoryGeneration {
    pub(crate) fn seal_cursor<P: Serialize>(
        &self,
        operation: &str,
        request_digest: &str,
        position: P,
    ) -> Result<String> {
        let payload = serde_json::to_vec(&(
            CURSOR_VERSION,
            digest(self.repository_identity()),
            digest(self.database_incarnation_id()),
            self.generation(),
            digest(self.semantics_fingerprint()),
            operation,
            request_digest,
            position,
        ))?;
        let tag = cursor_integrity(&payload);
        Ok(format!(
            "c2.{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            compact_hash(tag, 16)
        ))
    }

    pub(crate) fn open_cursor<P: DeserializeOwned>(
        &self,
        token: &str,
        operation: &str,
        request_digest: &str,
    ) -> Result<P> {
        let (prefix, payload, tag) = split_token(token).ok_or(Error::StaleCursor)?;
        if prefix != "c2" {
            return Err(Error::StaleCursor);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Error::StaleCursor)?;
        let supplied_tag = URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| Error::StaleCursor)?;
        let expected_tag = cursor_integrity(&payload);
        if !constant_time_eq(&supplied_tag, &expected_tag.as_bytes()[..16]) {
            return Err(Error::StaleCursor);
        }
        let (
            version,
            repository,
            incarnation,
            generation,
            semantics,
            encoded_operation,
            request,
            position,
        ): (u8, String, String, u64, String, String, String, P) =
            serde_json::from_slice(&payload).map_err(|_| Error::StaleCursor)?;
        if version != CURSOR_VERSION
            || repository != digest(self.repository_identity())
            || incarnation != digest(self.database_incarnation_id())
            || generation != self.generation()
            || semantics != digest(self.semantics_fingerprint())
            || encoded_operation != operation
            || request != request_digest
        {
            return Err(Error::StaleCursor);
        }
        Ok(position)
    }
}

fn digest(value: &str) -> String {
    compact_hash(blake3::hash(value.as_bytes()), 12)
}

fn compact_hash(hash: blake3::Hash, bytes: usize) -> String {
    URL_SAFE_NO_PAD.encode(&hash.as_bytes()[..bytes])
}

fn cursor_integrity(payload: &[u8]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leantoken-cursor-v1\0");
    hasher.update(payload);
    hasher.finalize()
}

fn split_token(token: &str) -> Option<(&str, &str, &str)> {
    let mut fields = token.split('.');
    let result = (fields.next()?, fields.next()?, fields.next()?);
    fields.next().is_none().then_some(result)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    #[test]
    fn cursor_rejects_a_recreated_database_at_the_same_generation() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("index.sqlite");
        let storage = Storage::open(&path).expect("first storage");
        storage
            .full_reconcile("projection", Vec::new())
            .expect("first generation");
        let first = RepositoryGeneration::open(&storage).expect("first generation snapshot");
        let cursor = first
            .seal_cursor("fixture", "request", 7_usize)
            .expect("sealed cursor");
        drop(first);
        drop(storage);

        for suffix in ["", "-wal", "-shm", "-journal"] {
            let artifact = std::path::PathBuf::from(format!("{}{}", path.display(), suffix));
            if artifact.exists() {
                std::fs::remove_file(artifact).expect("remove recreated database artifact");
            }
        }

        let replacement = Storage::open(&path).expect("replacement storage");
        replacement
            .full_reconcile("projection", Vec::new())
            .expect("replacement first generation");
        let replacement = RepositoryGeneration::open(&replacement).expect("replacement snapshot");
        assert!(matches!(
            replacement.open_cursor::<usize>(&cursor, "fixture", "request"),
            Err(Error::StaleCursor)
        ));
    }
}
