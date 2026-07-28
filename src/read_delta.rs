pub(crate) const MAX_READ_DELTA_BASES: usize = 128;
pub(crate) const MAX_READ_DELTA_BASE_BYTES: usize = 512 * 1024;
pub(crate) const MAX_TOTAL_READ_DELTA_BASE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const READ_DELTA_BASE_TTL_MILLIS: i64 = 30 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadDeltaBase {
    pub content: String,
    pub generation: u64,
    pub target_start_line: usize,
    pub target_end_line: usize,
    pub returned_start_line: usize,
    pub returned_end_line: usize,
}

impl ReadDeltaBase {
    pub(crate) fn logical_bytes(&self, target_key: &str, content_hash: &str) -> usize {
        (7 * size_of::<u64>())
            .saturating_add(target_key.len())
            .saturating_add(content_hash.len())
            .saturating_add(self.content.len())
    }
}
