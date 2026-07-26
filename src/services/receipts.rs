use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use super::Services;
use crate::model::ResponseMeta;
use crate::{Error, Result};

const MAX_RECEIPTS: usize = 128;
const MAX_EVIDENCE_PER_RECEIPT: usize = 2_048;
const MAX_TOTAL_EVIDENCE: usize = 16_384;
const MAX_RECEIPT_ID_BYTES: usize = 128;
const NEAR_DUPLICATE_HAMMING_DISTANCE: u32 = 8;

#[derive(Debug, Clone)]
pub(super) struct ReceiptEvidence {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content_hash: String,
    semantic_signature: Option<u64>,
}

impl ReceiptEvidence {
    pub(super) fn new(
        path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content_hash: impl Into<String>,
        content: Option<&str>,
    ) -> Self {
        Self {
            path: path.into(),
            start_line,
            end_line,
            content_hash: content_hash.into(),
            semantic_signature: content.and_then(semantic_signature),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReceiptDecision {
    Return,
    SuppressExact,
    SuppressOverlap,
    ReturnNearDuplicate,
}

#[derive(Debug)]
pub(super) struct ReceiptEvaluation {
    pub receipt_id: String,
    pub decisions: Vec<ReceiptDecision>,
}

impl ReceiptEvaluation {
    pub(super) fn apply_meta(&self, meta: &mut ResponseMeta) {
        meta.receipt_id = Some(self.receipt_id.clone());
        meta.receipt_suppressed_exact = self
            .decisions
            .iter()
            .filter(|decision| matches!(decision, ReceiptDecision::SuppressExact))
            .count();
        meta.receipt_suppressed_overlap = self
            .decisions
            .iter()
            .filter(|decision| matches!(decision, ReceiptDecision::SuppressOverlap))
            .count();
        meta.receipt_near_duplicates = self
            .decisions
            .iter()
            .filter(|decision| matches!(decision, ReceiptDecision::ReturnNearDuplicate))
            .count();
    }
}

impl Services {
    pub(super) fn evaluate_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.receipts.evaluate(
            requested_id,
            generation,
            || {
                let id = self.next_receipt_id.fetch_add(1, Ordering::Relaxed);
                format!("r{id:016x}")
            },
            candidates,
        )
    }

    pub(super) fn evaluate_read_receipt(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.receipts.evaluate_exact_only(
            requested_id,
            generation,
            || {
                let id = self.next_receipt_id.fetch_add(1, Ordering::Relaxed);
                format!("r{id:016x}")
            },
            candidates,
        )
    }
}

#[derive(Debug)]
struct ReceiptState {
    generation: u64,
    evidence: Vec<ReceiptEvidence>,
}

#[derive(Debug, Default)]
struct RegistryState {
    receipts: HashMap<String, ReceiptState>,
    insertion_order: VecDeque<String>,
}

#[derive(Debug, Default)]
pub(super) struct ReceiptRegistry {
    state: Mutex<RegistryState>,
}

impl ReceiptRegistry {
    pub(super) fn evaluate(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        generated_id: impl FnOnce() -> String,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.evaluate_with_overlap_policy(requested_id, generation, generated_id, candidates, true)
    }

    pub(super) fn evaluate_exact_only(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        generated_id: impl FnOnce() -> String,
        candidates: &[ReceiptEvidence],
    ) -> Result<ReceiptEvaluation> {
        self.evaluate_with_overlap_policy(requested_id, generation, generated_id, candidates, false)
    }

    fn evaluate_with_overlap_policy(
        &self,
        requested_id: Option<&str>,
        generation: u64,
        generated_id: impl FnOnce() -> String,
        candidates: &[ReceiptEvidence],
        suppress_overlap: bool,
    ) -> Result<ReceiptEvaluation> {
        if requested_id.is_some_and(|id| id.len() > MAX_RECEIPT_ID_BYTES) {
            return Err(Error::InputTooLong {
                field: "receipt_id",
                max_bytes: MAX_RECEIPT_ID_BYTES,
            });
        }
        let mut registry = self
            .state
            .lock()
            .map_err(|_| Error::InternalFailure("retrieval receipt registry poisoned".into()))?;
        let receipt_id = requested_id.map_or_else(generated_id, str::to_owned);

        if let Some(receipt) = registry.receipts.get(&receipt_id)
            && receipt.generation != generation
        {
            return Err(Error::StaleReceipt {
                receipt_generation: receipt.generation,
                repository_generation: generation,
            });
        }
        if requested_id.is_some() && !registry.receipts.contains_key(&receipt_id) {
            return Err(Error::UnknownReceipt(receipt_id));
        }

        let decisions = registry
            .receipts
            .get(&receipt_id)
            .map(|receipt| {
                candidates
                    .iter()
                    .map(|candidate| {
                        let decision = decide(&receipt.evidence, candidate);
                        if !suppress_overlap && decision == ReceiptDecision::SuppressOverlap {
                            ReceiptDecision::Return
                        } else {
                            decision
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![ReceiptDecision::Return; candidates.len()]);

        if !registry.receipts.contains_key(&receipt_id) {
            while registry.receipts.len() >= MAX_RECEIPTS {
                if let Some(expired) = registry.insertion_order.pop_front() {
                    registry.receipts.remove(&expired);
                }
            }
            registry.insertion_order.push_back(receipt_id.clone());
            registry.receipts.insert(
                receipt_id.clone(),
                ReceiptState {
                    generation,
                    evidence: Vec::new(),
                },
            );
        }
        let returned = candidates
            .iter()
            .zip(&decisions)
            .filter(|(_, decision)| {
                matches!(
                    decision,
                    ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                )
            })
            .map(|(candidate, _)| candidate.clone())
            .collect::<Vec<_>>();
        let current_len = registry
            .receipts
            .get(&receipt_id)
            .map_or(0, |receipt| receipt.evidence.len());
        let desired = returned
            .len()
            .min(MAX_EVIDENCE_PER_RECEIPT.saturating_sub(current_len));
        while registry.total_evidence().saturating_add(desired) > MAX_TOTAL_EVIDENCE {
            if !registry.evict_oldest_except(&receipt_id) {
                break;
            }
        }
        let available = MAX_TOTAL_EVIDENCE.saturating_sub(registry.total_evidence());
        if let Some(receipt) = registry.receipts.get_mut(&receipt_id) {
            receipt
                .evidence
                .extend(returned.into_iter().take(desired.min(available)));
        }

        Ok(ReceiptEvaluation {
            receipt_id,
            decisions,
        })
    }
}

impl RegistryState {
    fn total_evidence(&self) -> usize {
        self.receipts
            .values()
            .map(|receipt| receipt.evidence.len())
            .sum()
    }

    fn evict_oldest_except(&mut self, retained_id: &str) -> bool {
        for _ in 0..self.insertion_order.len() {
            let Some(candidate) = self.insertion_order.pop_front() else {
                return false;
            };
            if candidate == retained_id {
                self.insertion_order.push_back(candidate);
                continue;
            }
            self.receipts.remove(&candidate);
            return true;
        }
        false
    }
}

fn decide(previous: &[ReceiptEvidence], candidate: &ReceiptEvidence) -> ReceiptDecision {
    if !candidate.content_hash.is_empty()
        && previous
            .iter()
            .any(|seen| seen.content_hash == candidate.content_hash)
    {
        return ReceiptDecision::SuppressExact;
    }
    if previous.iter().any(|seen| {
        seen.path == candidate.path
            && ranges_overlap(
                seen.start_line,
                seen.end_line,
                candidate.start_line,
                candidate.end_line,
            )
    }) {
        return ReceiptDecision::SuppressOverlap;
    }
    if candidate.semantic_signature.is_some_and(|signature| {
        previous.iter().any(|seen| {
            seen.semantic_signature.is_some_and(|prior| {
                (signature ^ prior).count_ones() <= NEAR_DUPLICATE_HAMMING_DISTANCE
            })
        })
    }) {
        return ReceiptDecision::ReturnNearDuplicate;
    }
    ReceiptDecision::Return
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start <= right_end && right_start <= left_end
}

fn semantic_signature(content: &str) -> Option<u64> {
    let tokens = content
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| token.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if tokens.len() < 3 {
        return None;
    }
    let mut weights = [0i32; 64];
    for token in tokens {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        token.hash(&mut hasher);
        let hash = hasher.finish();
        for (bit, weight) in weights.iter_mut().enumerate() {
            if hash & (1u64 << bit) == 0 {
                *weight -= 1;
            } else {
                *weight += 1;
            }
        }
    }
    Some(
        weights
            .iter()
            .enumerate()
            .fold(0u64, |signature, (bit, weight)| {
                signature | (u64::from(*weight >= 0) << bit)
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_suppresses_exact_then_overlap_and_only_warns_for_near_duplicates() {
        let registry = ReceiptRegistry::default();
        let first = ReceiptEvidence::new(
            "src/lib.rs",
            10,
            20,
            "first",
            Some("alpha beta gamma delta epsilon"),
        );
        let created = registry
            .evaluate(None, 7, || "r1".into(), std::slice::from_ref(&first))
            .expect("create");
        assert_eq!(created.receipt_id, "r1");

        let candidates = vec![
            first,
            ReceiptEvidence::new("src/lib.rs", 20, 30, "second", Some("unrelated words here")),
            ReceiptEvidence::new(
                "src/other.rs",
                1,
                2,
                "third",
                Some("alpha beta gamma delta epsilon zeta"),
            ),
        ];
        let repeated = registry
            .evaluate(Some("r1"), 7, || unreachable!(), &candidates)
            .expect("reuse");
        assert_eq!(
            repeated.decisions,
            vec![
                ReceiptDecision::SuppressExact,
                ReceiptDecision::SuppressOverlap,
                ReceiptDecision::ReturnNearDuplicate,
            ]
        );
    }

    #[test]
    fn receipt_rejects_unknown_and_stale_ids() {
        let registry = ReceiptRegistry::default();
        assert!(matches!(
            registry.evaluate(Some("missing"), 1, || unreachable!(), &[]),
            Err(Error::UnknownReceipt(id)) if id == "missing"
        ));
        registry
            .evaluate(None, 1, || "r1".into(), &[])
            .expect("create");
        assert!(matches!(
            registry.evaluate(Some("r1"), 2, || unreachable!(), &[]),
            Err(Error::StaleReceipt {
                receipt_generation: 1,
                repository_generation: 2
            })
        ));
    }

    #[test]
    fn receipt_registry_evicts_the_oldest_session_at_its_bound() {
        let registry = ReceiptRegistry::default();
        for index in 0..=MAX_RECEIPTS {
            registry
                .evaluate(None, 1, || format!("r{index}"), &[])
                .expect("create receipt");
        }
        assert!(matches!(
            registry.evaluate(Some("r0"), 1, || unreachable!(), &[]),
            Err(Error::UnknownReceipt(id)) if id == "r0"
        ));
        registry
            .evaluate(Some(&format!("r{MAX_RECEIPTS}")), 1, || unreachable!(), &[])
            .expect("newest receipt remains available");
    }
}
