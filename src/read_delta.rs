pub(crate) const MAX_READ_DELTA_BASE_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReadDeltaBase {
    pub target_key: String,
    pub content_hash: String,
    pub content: String,
    pub generation: u64,
    pub target_start_line: usize,
    pub target_end_line: usize,
    pub returned_start_line: usize,
    pub returned_end_line: usize,
}
