use super::*;

/// Default number of deterministic examples returned for each rebase outcome.
pub const DEFAULT_RECEIPT_REBASE_SAMPLES_PER_OUTCOME: usize = 4;
/// Maximum deterministic examples returned for each rebase outcome.
pub const MAX_RECEIPT_REBASE_SAMPLES_PER_OUTCOME: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Explicit request to carry only exactly unchanged evidence into a new generation.
pub struct ReceiptRebaseRequest {
    /// Opaque server-managed receipt from an earlier repository generation.
    pub receipt_id: String,
    /// Maximum examples retained for each outcome; counts and the digest remain complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_samples_per_outcome: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Exact-only disposition of one source receipt evidence item.
pub enum ReceiptRebaseOutcomeKind {
    /// Path, coordinates, and emitted-content hash all match the current generation.
    Carried,
    /// The path and coordinates still exist, but their exact evidence hash changed.
    Changed,
    /// The original repository-relative path is absent from the current generation.
    Missing,
    /// The current generation cannot prove an exact coordinate mapping.
    Unmapped,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// One bounded, source-free example from a complete rebase classification.
pub struct ReceiptRebaseSample {
    /// Original evidence ordinal in the source receipt.
    pub ordinal: usize,
    /// Original normalized repository-relative path.
    pub path: String,
    /// Original one-based inclusive start line.
    pub start_line: usize,
    /// Original one-based inclusive end line.
    pub end_line: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Complete counts for every exact-only rebase outcome.
pub struct ReceiptRebaseCounts {
    /// Evidence proven identical and copied into the new receipt.
    pub carried: usize,
    /// Evidence whose exact old coordinates now produce different content.
    pub changed: usize,
    /// Evidence whose old path is absent from the current generation.
    pub missing: usize,
    /// Evidence that could not be proven exact within the validation bounds.
    pub unmapped: usize,
}

impl ReceiptRebaseCounts {
    /// Return the complete number of classified source evidence items.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.carried + self.changed + self.missing + self.unmapped
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
/// Deterministic bounded examples grouped by exact-only outcome.
pub struct ReceiptRebaseSamples {
    /// Bounded carried examples in source-receipt ordinal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<ReceiptRebaseSample>,
    /// Bounded changed examples in source-receipt ordinal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<ReceiptRebaseSample>,
    /// Bounded missing examples in source-receipt ordinal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<ReceiptRebaseSample>,
    /// Bounded unmapped examples in source-receipt ordinal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmapped: Vec<ReceiptRebaseSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
/// Bounded report for an atomic exact-only receipt rebase.
pub struct ReceiptRebaseResponse {
    /// Immutable source receipt accepted for this rebase.
    pub source_receipt_id: String,
    /// Repository generation recorded on the source receipt.
    pub source_repository_generation: u64,
    /// Complete outcome counts across the source receipt.
    pub counts: ReceiptRebaseCounts,
    /// Bounded source-free examples; full classification remains committed by the digest.
    pub samples: ReceiptRebaseSamples,
    /// Whether `samples` contains every classified source evidence item.
    pub samples_complete: bool,
    /// BLAKE3 commitment to the ordered complete classification and generation binding.
    pub outcomes_blake3: String,
    /// Current repository metadata; `receipt_id` is the new current-generation receipt.
    pub meta: ResponseMeta,
}
