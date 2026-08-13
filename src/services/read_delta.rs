use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use similar::TextDiff;

use crate::model::{
    ReadDeltaBaseSource, ReadDeltaFallback, ReadDeltaOutcome, ReadDeltaPersistenceFallback,
    ReadDeltaReceipt, ReadResponse,
};
use crate::read_delta::{
    MAX_READ_DELTA_BASE_BYTES as MAX_READ_DELTA_ENTRY_BYTES,
    MAX_READ_DELTA_BASES as MAX_READ_DELTA_ENTRIES,
    MAX_TOTAL_READ_DELTA_BASE_BYTES as MAX_READ_DELTA_TOTAL_BYTES, ReadDeltaBase,
};
use crate::storage::Storage;
use crate::tokens::Tokenizer;
use crate::{Error, Result};

use super::read::NewReadTarget;

const READ_DELTA_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    target_key: String,
    content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DeltaCacheKey {
    target_key: String,
    base_hash: String,
    head_hash: String,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    content: String,
    generation: u64,
    target_start_line: usize,
    target_end_line: usize,
    returned_start_line: usize,
    returned_end_line: usize,
    inserted_at: Instant,
}

#[derive(Debug, Clone)]
struct DeltaCacheEntry {
    delta: String,
    inserted_at: Instant,
}

#[derive(Debug, Default)]
struct RegistryState {
    entries: HashMap<CacheKey, CacheEntry>,
    insertion_order: VecDeque<CacheKey>,
    total_bytes: usize,
    deltas: HashMap<DeltaCacheKey, DeltaCacheEntry>,
    delta_order: VecDeque<DeltaCacheKey>,
    delta_bytes: usize,
}

#[derive(Debug, Default)]
pub(super) struct ReadDeltaRegistry {
    state: Mutex<RegistryState>,
}

pub(super) struct ReadDeltaEvaluation {
    pub delta: Option<String>,
    pub receipt: ReadDeltaReceipt,
}

pub(super) struct ReadDeltaInput<'a> {
    pub repository_id: &'a str,
    pub storage: &'a Storage,
    pub path: &'a str,
    pub target: &'a NewReadTarget,
    pub expected_hash: Option<&'a str>,
    pub response: &'a ReadResponse,
    pub current_content: &'a str,
    pub full_tokens: usize,
    pub tokenizer: Tokenizer,
}

impl ReadDeltaRegistry {
    pub(super) fn evaluate(&self, input: ReadDeltaInput<'_>) -> Result<ReadDeltaEvaluation> {
        let ReadDeltaInput {
            repository_id,
            storage,
            path,
            target,
            expected_hash,
            response,
            current_content,
            full_tokens,
            tokenizer,
        } = input;
        let target_key = target_key(repository_id, path, target);
        let mut base_hash = expected_hash.map(str::to_owned);
        let mut base_generation = None;
        let mut base_source = None;
        let mut delta = None;
        let mut outcome = ReadDeltaOutcome::Full;
        let mut delta_tokens = None;
        let mut avoided_tokens = 0;
        let mut fallback_reason = None;

        if expected_hash == Some(response.content_hash.as_str()) {
            outcome = ReadDeltaOutcome::NotModified;
            delta_tokens = Some(0);
            avoided_tokens = full_tokens;
        } else if response.truncated {
            fallback_reason = Some(ReadDeltaFallback::CurrentTruncated);
        } else if current_content.len() > MAX_READ_DELTA_ENTRY_BYTES {
            fallback_reason = Some(ReadDeltaFallback::ContentTooLarge);
        } else {
            let base = if let Some(expected_hash) = expected_hash {
                if let Some(entry) = self.lookup(&target_key, expected_hash)? {
                    Some((
                        expected_hash.to_owned(),
                        entry,
                        ReadDeltaBaseSource::ProcessLocal,
                    ))
                } else {
                    storage
                        .read_delta_base(&target_key, expected_hash)?
                        .map(|base| {
                            (
                                expected_hash.to_owned(),
                                CacheEntry::from_persistent(base),
                                ReadDeltaBaseSource::Persistent,
                            )
                        })
                }
            } else {
                let local = self.lookup_latest(&target_key)?;
                let persistent = storage.latest_read_delta_base(&target_key)?;
                match (local, persistent) {
                    (Some((hash, entry)), Some((persistent_hash, persistent))) => {
                        if entry.generation >= persistent.generation {
                            Some((hash, entry, ReadDeltaBaseSource::ProcessLocal))
                        } else {
                            Some((
                                persistent_hash,
                                CacheEntry::from_persistent(persistent),
                                ReadDeltaBaseSource::Persistent,
                            ))
                        }
                    }
                    (Some((hash, entry)), None) => {
                        Some((hash, entry, ReadDeltaBaseSource::ProcessLocal))
                    }
                    (None, Some((hash, persistent))) => Some((
                        hash,
                        CacheEntry::from_persistent(persistent),
                        ReadDeltaBaseSource::Persistent,
                    )),
                    (None, None) => None,
                }
            };
            if let Some((selected_hash, base, selected_source)) = base {
                base_hash = Some(selected_hash.clone());
                base_generation = Some(base.generation);
                base_source = Some(selected_source);
                if base_hash.as_deref() == Some(response.content_hash.as_str()) {
                    outcome = ReadDeltaOutcome::NotModified;
                    delta_tokens = Some(0);
                    avoided_tokens = full_tokens;
                } else if target_coordinates_changed(&base, response) {
                    fallback_reason = Some(ReadDeltaFallback::TargetChanged);
                } else {
                    let full_delta = if let Some(cached) =
                        self.lookup_delta(&target_key, &selected_hash, &response.content_hash)?
                    {
                        cached
                    } else {
                        let computed = TextDiff::from_lines(base.content.as_str(), current_content)
                            .unified_diff()
                            .context_radius(3)
                            .header(
                                &format!(
                                    "base/{}:{}-{}",
                                    response.path,
                                    response.returned_start_line,
                                    response.returned_end_line
                                ),
                                &format!(
                                    "head/{}:{}-{}",
                                    response.path,
                                    response.returned_start_line,
                                    response.returned_end_line
                                ),
                            )
                            .to_string();
                        self.insert_delta(
                            DeltaCacheKey {
                                target_key: target_key.clone(),
                                base_hash: selected_hash,
                                head_hash: response.content_hash.clone(),
                            },
                            computed.clone(),
                        )?;
                        computed
                    };
                    let candidate_tokens = tokenizer.count(&full_delta);
                    if candidate_tokens < full_tokens {
                        outcome = ReadDeltaOutcome::Delta;
                        delta_tokens = Some(candidate_tokens);
                        avoided_tokens = full_tokens - candidate_tokens;
                        delta = Some(full_delta);
                    } else {
                        fallback_reason = Some(ReadDeltaFallback::DeltaNotSmaller);
                    }
                }
            } else {
                fallback_reason = Some(ReadDeltaFallback::BaseUnavailable);
            }
        }

        let persistent_base = ReadDeltaBase {
            content: current_content.to_owned(),
            generation: response.meta.repository_generation,
            target_start_line: response.target_start_line,
            target_end_line: response.target_end_line,
            returned_start_line: response.returned_start_line,
            returned_end_line: response.returned_end_line,
        };
        if !response.truncated && current_content.len() <= MAX_READ_DELTA_ENTRY_BYTES {
            self.insert(
                CacheKey {
                    target_key: target_key.clone(),
                    content_hash: response.content_hash.clone(),
                },
                CacheEntry {
                    content: current_content.to_owned(),
                    generation: response.meta.repository_generation,
                    target_start_line: response.target_start_line,
                    target_end_line: response.target_end_line,
                    returned_start_line: response.returned_start_line,
                    returned_end_line: response.returned_end_line,
                    inserted_at: Instant::now(),
                },
            )?;
        }
        let mut persistence_fallback_reason = if response.truncated {
            Some(ReadDeltaPersistenceFallback::CurrentTruncated)
        } else if current_content.len() > MAX_READ_DELTA_ENTRY_BYTES {
            Some(ReadDeltaPersistenceFallback::ContentTooLarge)
        } else if response.index_stale {
            Some(ReadDeltaPersistenceFallback::LiveDiffersFromIndex)
        } else if response.indexed_hash.is_none() {
            Some(ReadDeltaPersistenceFallback::IndexedHashUnavailable)
        } else {
            None
        };
        let head_persisted = if persistence_fallback_reason.is_none() {
            let persisted = storage.persist_read_delta_base(
                &target_key,
                &response.content_hash,
                &persistent_base,
            )?;
            if !persisted {
                persistence_fallback_reason = Some(ReadDeltaPersistenceFallback::StorageCapacity);
            }
            persisted
        } else {
            false
        };

        Ok(ReadDeltaEvaluation {
            delta,
            receipt: ReadDeltaReceipt {
                target_key,
                base_hash,
                head_hash: response.content_hash.clone(),
                base_generation,
                base_source,
                head_generation: response.meta.repository_generation,
                outcome,
                full_tokens,
                delta_tokens,
                avoided_tokens,
                head_persisted,
                persistence_fallback_reason,
                fallback_reason,
            },
        })
    }

    fn lookup(&self, target_key: &str, content_hash: &str) -> Result<Option<CacheEntry>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::OperationFailure("read delta registry poisoned".into()))?;
        prune_expired(&mut state, Instant::now());
        Ok(state
            .entries
            .get(&CacheKey {
                target_key: target_key.to_owned(),
                content_hash: content_hash.to_owned(),
            })
            .cloned())
    }

    fn lookup_latest(&self, target_key: &str) -> Result<Option<(String, CacheEntry)>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::OperationFailure("read delta registry poisoned".into()))?;
        prune_expired(&mut state, Instant::now());
        Ok(state.insertion_order.iter().rev().find_map(|key| {
            if key.target_key != target_key {
                return None;
            }
            state
                .entries
                .get(key)
                .cloned()
                .map(|entry| (key.content_hash.clone(), entry))
        }))
    }

    fn lookup_delta(
        &self,
        target_key: &str,
        base_hash: &str,
        head_hash: &str,
    ) -> Result<Option<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::OperationFailure("read delta registry poisoned".into()))?;
        prune_expired(&mut state, Instant::now());
        Ok(state
            .deltas
            .get(&DeltaCacheKey {
                target_key: target_key.to_owned(),
                base_hash: base_hash.to_owned(),
                head_hash: head_hash.to_owned(),
            })
            .map(|entry| entry.delta.clone()))
    }

    fn insert(&self, key: CacheKey, entry: CacheEntry) -> Result<()> {
        if entry.content.len() > MAX_READ_DELTA_ENTRY_BYTES {
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::OperationFailure("read delta registry poisoned".into()))?;
        prune_expired(&mut state, Instant::now());
        if let Some(previous) = state.entries.remove(&key) {
            state.total_bytes = state.total_bytes.saturating_sub(previous.content.len());
            state.insertion_order.retain(|candidate| candidate != &key);
        }
        while state.entries.len() >= MAX_READ_DELTA_ENTRIES
            || state.total_bytes.saturating_add(entry.content.len()) > MAX_READ_DELTA_TOTAL_BYTES
        {
            if !evict_oldest(&mut state) {
                break;
            }
        }
        if state.entries.len() < MAX_READ_DELTA_ENTRIES
            && state.total_bytes.saturating_add(entry.content.len()) <= MAX_READ_DELTA_TOTAL_BYTES
        {
            state.total_bytes += entry.content.len();
            state.insertion_order.push_back(key.clone());
            state.entries.insert(key, entry);
        }
        Ok(())
    }

    fn insert_delta(&self, key: DeltaCacheKey, delta: String) -> Result<()> {
        if delta.len() > MAX_READ_DELTA_ENTRY_BYTES {
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::OperationFailure("read delta registry poisoned".into()))?;
        prune_expired(&mut state, Instant::now());
        if let Some(previous) = state.deltas.remove(&key) {
            state.delta_bytes = state.delta_bytes.saturating_sub(previous.delta.len());
            state.delta_order.retain(|candidate| candidate != &key);
        }
        while state.deltas.len() >= MAX_READ_DELTA_ENTRIES
            || state.delta_bytes.saturating_add(delta.len()) > MAX_READ_DELTA_TOTAL_BYTES
        {
            if !evict_oldest_delta(&mut state) {
                break;
            }
        }
        if state.deltas.len() < MAX_READ_DELTA_ENTRIES
            && state.delta_bytes.saturating_add(delta.len()) <= MAX_READ_DELTA_TOTAL_BYTES
        {
            state.delta_bytes += delta.len();
            state.delta_order.push_back(key.clone());
            state.deltas.insert(
                key,
                DeltaCacheEntry {
                    delta,
                    inserted_at: Instant::now(),
                },
            );
        }
        Ok(())
    }
}

impl CacheEntry {
    fn from_persistent(base: ReadDeltaBase) -> Self {
        Self {
            content: base.content,
            generation: base.generation,
            target_start_line: base.target_start_line,
            target_end_line: base.target_end_line,
            returned_start_line: base.returned_start_line,
            returned_end_line: base.returned_end_line,
            inserted_at: Instant::now(),
        }
    }
}

fn target_key(repository_id: &str, path: &str, target: &NewReadTarget) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [repository_id, path] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    match target {
        NewReadTarget::Symbol(symbol) => {
            hasher.update(b"symbol\0");
            hasher.update(symbol.as_bytes());
        }
        NewReadTarget::Heading { name, occurrence } => {
            hasher.update(b"heading\0");
            hasher.update(name.as_bytes());
            hasher.update(&[0]);
            hasher.update(
                &u64::try_from(occurrence.get())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
        }
        NewReadTarget::Lines { start, end } => {
            hasher.update(b"lines\0");
            hasher.update(&u64::try_from(start.get()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(
                &end.and_then(|line| u64::try_from(line).ok())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn target_coordinates_changed(base: &CacheEntry, head: &ReadResponse) -> bool {
    base.target_start_line != head.target_start_line
        || base.target_end_line != head.target_end_line
        || base.returned_start_line != head.returned_start_line
        || base.returned_end_line != head.returned_end_line
}

fn prune_expired(state: &mut RegistryState, now: Instant) {
    while let Some(key) = state.insertion_order.front() {
        let expired = state
            .entries
            .get(key)
            .is_none_or(|entry| now.duration_since(entry.inserted_at) >= READ_DELTA_TTL);
        if !expired {
            break;
        }
        evict_oldest(state);
    }
    while let Some(key) = state.delta_order.front() {
        let expired = state
            .deltas
            .get(key)
            .is_none_or(|entry| now.duration_since(entry.inserted_at) >= READ_DELTA_TTL);
        if !expired {
            break;
        }
        evict_oldest_delta(state);
    }
}

fn evict_oldest(state: &mut RegistryState) -> bool {
    let Some(key) = state.insertion_order.pop_front() else {
        return false;
    };
    if let Some(entry) = state.entries.remove(&key) {
        state.total_bytes = state.total_bytes.saturating_sub(entry.content.len());
    }
    true
}

fn evict_oldest_delta(state: &mut RegistryState) -> bool {
    let Some(key) = state.delta_order.pop_front() else {
        return false;
    };
    if let Some(entry) = state.deltas.remove(&key) {
        state.delta_bytes = state.delta_bytes.saturating_sub(entry.delta.len());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Freshness, ResponseMeta};

    fn entry(content: &str, inserted_at: Instant) -> CacheEntry {
        CacheEntry {
            content: content.into(),
            generation: 1,
            target_start_line: 1,
            target_end_line: 1,
            returned_start_line: 1,
            returned_end_line: 1,
            inserted_at,
        }
    }

    #[test]
    fn registry_prunes_expired_entries_and_updates_byte_accounting() {
        let now = Instant::now();
        let key = CacheKey {
            target_key: "target".into(),
            content_hash: "hash".into(),
        };
        let mut state = RegistryState {
            total_bytes: 4,
            ..RegistryState::default()
        };
        state.insertion_order.push_back(key.clone());
        state.entries.insert(key, entry("base", now));

        prune_expired(&mut state, now + READ_DELTA_TTL + Duration::from_secs(1));

        assert!(state.entries.is_empty());
        assert!(state.insertion_order.is_empty());
        assert_eq!(state.total_bytes, 0);
    }

    #[test]
    fn registry_evicts_oldest_entries_at_count_bound() {
        let registry = ReadDeltaRegistry::default();
        for index in 0..=MAX_READ_DELTA_ENTRIES {
            registry
                .insert(
                    CacheKey {
                        target_key: format!("target-{index}"),
                        content_hash: format!("hash-{index}"),
                    },
                    entry("base", Instant::now()),
                )
                .expect("insert bounded delta base");
        }

        let state = registry.state.lock().expect("delta registry");
        assert_eq!(state.entries.len(), MAX_READ_DELTA_ENTRIES);
        assert_eq!(state.insertion_order.len(), MAX_READ_DELTA_ENTRIES);
        assert_eq!(state.total_bytes, MAX_READ_DELTA_ENTRIES * 4);
        assert!(
            !state.entries.contains_key(&CacheKey {
                target_key: "target-0".into(),
                content_hash: "hash-0".into(),
            }),
            "oldest entry must be evicted"
        );
    }

    #[test]
    fn registry_evicts_oldest_entries_at_total_byte_bound() {
        let registry = ReadDeltaRegistry::default();
        let content = "x".repeat(MAX_READ_DELTA_ENTRY_BYTES);
        let retained_entries = MAX_READ_DELTA_TOTAL_BYTES / MAX_READ_DELTA_ENTRY_BYTES;
        for index in 0..=retained_entries {
            registry
                .insert(
                    CacheKey {
                        target_key: format!("target-{index}"),
                        content_hash: format!("hash-{index}"),
                    },
                    entry(&content, Instant::now()),
                )
                .expect("insert byte-bounded delta base");
        }

        let state = registry.state.lock().expect("delta registry");
        assert_eq!(state.entries.len(), retained_entries);
        assert_eq!(state.insertion_order.len(), retained_entries);
        assert_eq!(state.total_bytes, MAX_READ_DELTA_TOTAL_BYTES);
        assert!(
            !state.entries.contains_key(&CacheKey {
                target_key: "target-0".into(),
                content_hash: "hash-0".into(),
            }),
            "oldest entry must be evicted"
        );
    }

    #[test]
    fn registry_refreshes_coordinates_and_ttl_for_an_existing_base() {
        let registry = ReadDeltaRegistry::default();
        let key = CacheKey {
            target_key: "target".into(),
            content_hash: "hash".into(),
        };
        registry
            .insert(
                key.clone(),
                entry("base", Instant::now() - Duration::from_secs(60)),
            )
            .expect("insert initial base");
        let mut refreshed = entry("base", Instant::now());
        refreshed.generation = 2;
        refreshed.target_start_line = 10;
        refreshed.target_end_line = 12;
        registry
            .insert(key.clone(), refreshed)
            .expect("refresh existing base");

        let state = registry.state.lock().expect("delta registry");
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.insertion_order.iter().collect::<Vec<_>>(), vec![&key]);
        assert_eq!(state.total_bytes, 4);
        let stored = state.entries.get(&key).expect("refreshed base");
        assert_eq!(stored.generation, 2);
        assert_eq!(stored.target_start_line, 10);
        assert_eq!(stored.target_end_line, 12);
    }

    #[test]
    fn registry_selects_the_latest_base_for_one_exact_target() {
        let registry = ReadDeltaRegistry::default();
        for (target, hash, generation) in [
            ("target", "old", 1),
            ("other", "unrelated", 9),
            ("target", "latest", 2),
        ] {
            let mut candidate = entry(hash, Instant::now());
            candidate.generation = generation;
            registry
                .insert(
                    CacheKey {
                        target_key: target.into(),
                        content_hash: hash.into(),
                    },
                    candidate,
                )
                .expect("insert delta base");
        }

        let (hash, latest) = registry
            .lookup_latest("target")
            .expect("latest lookup")
            .expect("matching target");
        assert_eq!(hash, "latest");
        assert_eq!(latest.generation, 2);
        assert_eq!(latest.content, "latest");
        assert!(
            registry
                .lookup_latest("missing")
                .expect("missing lookup")
                .is_none()
        );
    }

    #[test]
    fn registry_rejects_an_entry_above_the_per_entry_byte_bound() {
        let registry = ReadDeltaRegistry::default();
        let content = "x".repeat(MAX_READ_DELTA_ENTRY_BYTES + 1);
        registry
            .insert(
                CacheKey {
                    target_key: "target".into(),
                    content_hash: "hash".into(),
                },
                entry(&content, Instant::now()),
            )
            .expect("reject oversized delta base");

        let state = registry.state.lock().expect("delta registry");
        assert!(state.entries.is_empty());
        assert!(state.insertion_order.is_empty());
        assert_eq!(state.total_bytes, 0);
    }

    #[test]
    fn registry_prunes_expired_delta_entries() {
        let now = Instant::now();
        let key = DeltaCacheKey {
            target_key: "target".into(),
            base_hash: "base".into(),
            head_hash: "head".into(),
        };
        let mut state = RegistryState {
            delta_bytes: 3,
            ..RegistryState::default()
        };
        state.delta_order.push_back(key.clone());
        state.deltas.insert(
            key,
            DeltaCacheEntry {
                delta: "abc".into(),
                inserted_at: now,
            },
        );

        prune_expired(&mut state, now + READ_DELTA_TTL + Duration::from_secs(1));

        assert!(state.deltas.is_empty());
        assert!(state.delta_order.is_empty());
        assert_eq!(state.delta_bytes, 0);
    }

    #[test]
    fn oversized_complete_base_reports_both_process_and_persistence_fallbacks() {
        let directory = tempfile::tempdir().expect("directory");
        let storage = Storage::open(directory.path().join("index.sqlite")).expect("storage");
        let content = "x".repeat(MAX_READ_DELTA_ENTRY_BYTES + 1);
        let content_hash = crate::text::hash(&content);
        let target = NewReadTarget::Lines {
            start: std::num::NonZeroUsize::new(1).expect("one is non-zero"),
            end: None,
        };
        let response = ReadResponse {
            path: "oversized.rs".into(),
            status: crate::model::ReadStatus::Content,
            target_start_line: 1,
            target_end_line: 1,
            returned_start_line: 1,
            returned_end_line: 1,
            truncated: false,
            next_start_line: None,
            continuation_cursor: None,
            continuation_verification: None,
            truncation_guidance: None,
            not_modified: false,
            content: Some(content.clone()),
            delta: None,
            delta_receipt: None,
            content_hash,
            indexed_hash: Some("indexed".into()),
            index_stale: false,
            index_state: crate::model::ReadIndexState::Current,
            live_bytes_read: content.len(),
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 1,
                freshness: Freshness::Current,
                index_scope: crate::model::IndexScopeMode::Full,
                index_scope_digest: None,
                source_tokens: 0,
                protocol_tokens: 0,
                path_and_metadata_tokens: 0,
                total_response_tokens: 0,
                tokenizer: "cl100k_base".into(),
                token_count_exact: true,
                receipt_id: None,
                receipt_suppressed_exact: 0,
                receipt_suppressed_overlap: 0,
                receipt_near_duplicates: 0,
                next_cursor: None,
            },
        };
        let evaluation = ReadDeltaRegistry::default()
            .evaluate(ReadDeltaInput {
                repository_id: "repository",
                storage: &storage,
                path: &response.path,
                target: &target,
                expected_hash: None,
                response: &response,
                current_content: &content,
                full_tokens: 1,
                tokenizer: Tokenizer::Cl100kBase,
            })
            .expect("evaluate oversized base");
        assert_eq!(
            evaluation.receipt.fallback_reason,
            Some(ReadDeltaFallback::ContentTooLarge)
        );
        assert_eq!(
            evaluation.receipt.persistence_fallback_reason,
            Some(ReadDeltaPersistenceFallback::ContentTooLarge)
        );
        assert!(!evaluation.receipt.head_persisted);
    }
}
