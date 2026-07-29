use crate::model::ResponseMeta;

pub(crate) const MAX_RECEIPTS: usize = 128;
pub(crate) const MAX_EVIDENCE_PER_RECEIPT: usize = 2_048;
pub(crate) const MAX_TOTAL_EVIDENCE: usize = 16_384;
pub(crate) const MAX_RECEIPT_ID_BYTES: usize = 128;
pub(crate) const MAX_TOTAL_RECEIPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_EVIDENCE_BYTES_PER_RECEIPT: usize = 1024 * 1024;
pub(crate) const MAX_TOTAL_EVIDENCE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_REBASE_STRUCTURAL_CANDIDATES_PER_EVIDENCE: usize = 64;
pub(crate) const MAX_REBASE_LIVE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const RECEIPT_TTL_MILLIS: i64 = 24 * 60 * 60 * 1_000;
pub(crate) const RECEIPT_TOUCH_INTERVAL_MILLIS: i64 = 60 * 1_000;
const RECEIPT_ID_NAMESPACE_HEX_BYTES: usize = 32;
const RECEIPT_ID_ROW_HEX_BYTES: usize = 16;
// A valid, high-token-density ID used before storage assigns the exact opaque
// value. Keep the generated length assertion and tokenizer coverage together.
pub(crate) const RECEIPT_ID_RESPONSE_RESERVE: &str =
    "r0a1b2c3d4e5f60718293a4b5c6d7e8f901a2b3c4d5e6f708";
const NEAR_DUPLICATE_HAMMING_DISTANCE: u32 = 8;
const RECEIPT_EVIDENCE_FIXED_LOGICAL_BYTES: usize = 7 * size_of::<u64>();
const _: () = assert!(
    RECEIPT_ID_RESPONSE_RESERVE.len()
        == 1 + RECEIPT_ID_NAMESPACE_HEX_BYTES + RECEIPT_ID_ROW_HEX_BYTES
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiptEvidence {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content_hash: String,
    pub semantic_signature: Option<u64>,
    pub exact_only: bool,
}

impl ReceiptEvidence {
    pub(crate) fn new(
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
            exact_only: false,
        }
    }

    pub(crate) fn logical_bytes(&self) -> usize {
        RECEIPT_EVIDENCE_FIXED_LOGICAL_BYTES
            .saturating_add(self.path.len())
            .saturating_add(self.content_hash.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiptDecision {
    Return,
    SuppressExact,
    SuppressOverlap,
    ReturnNearDuplicate,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ReceiptEvaluation {
    pub receipt_id: String,
    pub decisions: Vec<ReceiptDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiptRebaseSource {
    pub receipt_id: String,
    pub repository_identity: String,
    pub repository_generation: u64,
    pub evidence: Vec<ReceiptEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredReceipt {
    pub receipt_id: String,
    pub repository_identity: String,
    pub repository_generation: u64,
    pub created_unix_millis: i64,
    pub expires_unix_millis: i64,
    pub evidence: Vec<ReceiptEvidence>,
}

impl ReceiptEvaluation {
    pub(crate) fn apply_meta(&self, meta: &mut ResponseMeta) {
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

pub(crate) fn decide(
    previous: &[ReceiptEvidence],
    candidate: &ReceiptEvidence,
    suppress_overlap: bool,
) -> ReceiptDecision {
    if !candidate.content_hash.is_empty()
        && previous
            .iter()
            .any(|seen| seen.content_hash == candidate.content_hash)
    {
        return ReceiptDecision::SuppressExact;
    }
    if suppress_overlap
        && previous.iter().any(|seen| {
            !seen.exact_only
                && seen.path == candidate.path
                && ranges_overlap(
                    seen.start_line,
                    seen.end_line,
                    candidate.start_line,
                    candidate.end_line,
                )
        })
    {
        return ReceiptDecision::SuppressOverlap;
    }
    if candidate.semantic_signature.is_some_and(|signature| {
        previous.iter().any(|seen| {
            !seen.exact_only
                && seen.semantic_signature.is_some_and(|prior| {
                    (signature ^ prior).count_ones() <= NEAR_DUPLICATE_HAMMING_DISTANCE
                })
        })
    }) {
        return ReceiptDecision::ReturnNearDuplicate;
    }
    ReceiptDecision::Return
}

pub(crate) fn format_receipt_id(namespace: &str, row_id: i64) -> String {
    format!("r{namespace}{row_id:016x}")
}

pub(crate) fn parse_receipt_id(receipt_id: &str, namespace: &str) -> Option<i64> {
    let expected_len = 1 + RECEIPT_ID_NAMESPACE_HEX_BYTES + RECEIPT_ID_ROW_HEX_BYTES;
    if receipt_id.len() != expected_len
        || !receipt_id.starts_with('r')
        || receipt_id.get(1..1 + RECEIPT_ID_NAMESPACE_HEX_BYTES)? != namespace
    {
        return None;
    }
    let row_id =
        i64::from_str_radix(receipt_id.get(1 + RECEIPT_ID_NAMESPACE_HEX_BYTES..)?, 16).ok()?;
    (row_id > 0).then_some(row_id)
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
        let digest = blake3::hash(token.as_bytes());
        let hash = u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("eight hash bytes"));
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
    fn decisions_preserve_exact_overlap_and_near_duplicate_order() {
        let first = ReceiptEvidence::new(
            "src/lib.rs",
            10,
            20,
            "first",
            Some("alpha beta gamma delta epsilon"),
        );
        let previous = vec![first.clone()];
        let candidates = [
            first,
            ReceiptEvidence::new("src/lib.rs", 20, 30, "second", Some("unrelated words here")),
            ReceiptEvidence::new(
                "src/other.rs",
                1,
                2,
                "third",
                Some("epsilon delta gamma beta alpha"),
            ),
        ];
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| decide(&previous, candidate, true))
                .collect::<Vec<_>>(),
            vec![
                ReceiptDecision::SuppressExact,
                ReceiptDecision::SuppressOverlap,
                ReceiptDecision::ReturnNearDuplicate,
            ]
        );
        assert_eq!(
            decide(&previous, &candidates[1], false),
            ReceiptDecision::Return
        );
    }

    #[test]
    fn exact_only_evidence_suppresses_hashes_but_not_ranges_or_signatures() {
        let mut previous = ReceiptEvidence::new(
            "src/lib.rs",
            10,
            20,
            "first",
            Some("alpha beta gamma delta epsilon"),
        );
        previous.exact_only = true;
        assert_eq!(
            decide(
                std::slice::from_ref(&previous),
                &ReceiptEvidence::new("src/lib.rs", 10, 20, "first", Some("replacement")),
                true,
            ),
            ReceiptDecision::SuppressExact
        );
        assert_eq!(
            decide(
                std::slice::from_ref(&previous),
                &ReceiptEvidence::new(
                    "src/lib.rs",
                    15,
                    25,
                    "changed",
                    Some("unrelated words here"),
                ),
                true,
            ),
            ReceiptDecision::Return
        );
        assert_eq!(
            decide(
                std::slice::from_ref(&previous),
                &ReceiptEvidence::new(
                    "src/other.rs",
                    1,
                    2,
                    "different",
                    Some("epsilon delta gamma beta alpha"),
                ),
                true,
            ),
            ReceiptDecision::Return
        );
    }

    #[test]
    fn receipt_ids_are_namespace_bound_and_input_bounded() {
        let namespace = "0123456789abcdef0123456789abcdef";
        let id = format_receipt_id(namespace, 42);
        assert!(id.len() <= MAX_RECEIPT_ID_BYTES);
        assert_eq!(parse_receipt_id(&id, namespace), Some(42));
        assert_eq!(
            parse_receipt_id(&id, "fedcba9876543210fedcba9876543210"),
            None
        );
        assert_eq!(parse_receipt_id("r1", namespace), None);
    }

    #[test]
    fn semantic_signatures_are_stable_across_processes_and_toolchains() {
        assert_eq!(
            semantic_signature("alpha beta gamma delta epsilon"),
            Some(7_562_433_588_066_552_642)
        );
    }

    #[test]
    fn response_reserve_covers_generated_ids_across_tokenizers() {
        use crate::tokens::Tokenizer;

        let tokenizers = [
            Tokenizer::Cl100kBase,
            Tokenizer::O200kBase,
            Tokenizer::O200kHarmony,
            Tokenizer::P50kBase,
            Tokenizer::R50kBase,
            Tokenizer::Gpt2,
            Tokenizer::P50kEdit,
            Tokenizer::Estimate,
        ];
        for seed in 0u64..4_096 {
            let namespace = blake3::hash(&seed.to_le_bytes()).to_hex();
            let id = format_receipt_id(
                &namespace.as_str()[..RECEIPT_ID_NAMESPACE_HEX_BYTES],
                i64::try_from(seed + 1).expect("bounded row id"),
            );
            for tokenizer in tokenizers {
                assert!(
                    tokenizer.count(&id) <= tokenizer.count(RECEIPT_ID_RESPONSE_RESERVE),
                    "{} under-reserved generated receipt {id}",
                    tokenizer.name()
                );
            }
        }
    }
}
