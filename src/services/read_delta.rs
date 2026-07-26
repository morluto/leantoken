use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use similar::TextDiff;

use crate::model::{
    ReadDeltaFallback, ReadDeltaOutcome, ReadDeltaReceipt, ReadRequest, ReadResponse,
};
use crate::tokens::Tokenizer;
use crate::{Error, Result};

const MAX_READ_DELTA_ENTRIES: usize = 128;
const MAX_READ_DELTA_ENTRY_BYTES: usize = 512 * 1024;
const MAX_READ_DELTA_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const READ_DELTA_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    target_key: String,
    content_hash: String,
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

#[derive(Debug, Default)]
struct RegistryState {
    entries: HashMap<CacheKey, CacheEntry>,
    insertion_order: VecDeque<CacheKey>,
    total_bytes: usize,
}

#[derive(Debug, Default)]
pub(super) struct ReadDeltaRegistry {
    state: Mutex<RegistryState>,
}

pub(super) struct ReadDeltaEvaluation {
    pub delta: Option<String>,
    pub receipt: ReadDeltaReceipt,
}

impl ReadDeltaRegistry {
    pub(super) fn evaluate(
        &self,
        repository_id: &str,
        request: &ReadRequest,
        response: &ReadResponse,
        current_content: &str,
        full_tokens: usize,
        tokenizer: Tokenizer,
    ) -> Result<ReadDeltaEvaluation> {
        let target_key = target_key(repository_id, request);
        let base_hash = request.expected_hash.clone();
        let mut base_generation = None;
        let mut delta = None;
        let mut outcome = ReadDeltaOutcome::Full;
        let mut delta_tokens = None;
        let mut avoided_tokens = 0;
        let mut fallback_reason = None;

        if request.expected_hash.as_deref() == Some(response.content_hash.as_str()) {
            outcome = ReadDeltaOutcome::NotModified;
            delta_tokens = Some(0);
            avoided_tokens = full_tokens;
        } else if response.truncated {
            fallback_reason = Some(ReadDeltaFallback::CurrentTruncated);
        } else if current_content.len() > MAX_READ_DELTA_ENTRY_BYTES {
            fallback_reason = Some(ReadDeltaFallback::ContentTooLarge);
        } else if let Some(expected_hash) = request.expected_hash.as_deref() {
            let base = self.lookup(&target_key, expected_hash)?;
            if let Some(base) = base {
                base_generation = Some(base.generation);
                if target_coordinates_changed(&base, response) {
                    fallback_reason = Some(ReadDeltaFallback::TargetChanged);
                } else {
                    let full_delta = TextDiff::from_lines(base.content.as_str(), current_content)
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

        Ok(ReadDeltaEvaluation {
            delta,
            receipt: ReadDeltaReceipt {
                target_key,
                base_hash,
                head_hash: response.content_hash.clone(),
                base_generation,
                head_generation: response.meta.repository_generation,
                outcome,
                full_tokens,
                delta_tokens,
                avoided_tokens,
                fallback_reason,
            },
        })
    }

    fn lookup(&self, target_key: &str, content_hash: &str) -> Result<Option<CacheEntry>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::InternalFailure("read delta registry poisoned".into()))?;
        prune_expired(&mut state, Instant::now());
        Ok(state
            .entries
            .get(&CacheKey {
                target_key: target_key.to_owned(),
                content_hash: content_hash.to_owned(),
            })
            .cloned())
    }

    fn insert(&self, key: CacheKey, entry: CacheEntry) -> Result<()> {
        if entry.content.len() > MAX_READ_DELTA_ENTRY_BYTES {
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::InternalFailure("read delta registry poisoned".into()))?;
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
}

fn target_key(repository_id: &str, request: &ReadRequest) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [repository_id, request.path.as_str()] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    if let Some(symbol) = request.symbol.as_deref() {
        hasher.update(b"symbol\0");
        hasher.update(symbol.as_bytes());
    } else if let Some(heading) = request.heading.as_deref() {
        hasher.update(b"heading\0");
        hasher.update(heading.as_bytes());
        hasher.update(&[0]);
        hasher.update(
            &u64::try_from(request.heading_occurrence.unwrap_or(1))
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
    } else {
        hasher.update(b"lines\0");
        hasher.update(
            &u64::try_from(request.start_line.unwrap_or(1))
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(
            &request
                .end_line
                .and_then(|line| u64::try_from(line).ok())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

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
        state.entries.insert(
            key,
            entry("base", now - READ_DELTA_TTL - Duration::from_secs(1)),
        );

        prune_expired(&mut state, now);

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
}
