// Private request, cursor, and materialization state shared by read stages.
#[derive(Clone)]
pub(in crate::services) struct StoredExcerpt {
    pub(in crate::services) content: String,
    pub(in crate::services) start_line: usize,
    pub(in crate::services) end_line: usize,
}

pub(in crate::services) struct StoredExcerptRequest {
    pub(in crate::services) file_id: i64,
    pub(in crate::services) desired_start_line: usize,
    pub(in crate::services) desired_end_line: usize,
    pub(in crate::services) required_start_line: usize,
    pub(in crate::services) required_end_line: usize,
    pub(in crate::services) max_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::services) struct ResolvedStoredExcerptRequest {
    pub(in crate::services) file_id: i64,
    pub(in crate::services) start_line: usize,
    pub(in crate::services) end_line: usize,
}

impl StoredExcerptRequest {
    pub(in crate::services) fn resolve(
        &self,
        file_end_line: Option<usize>,
    ) -> Option<ResolvedStoredExcerptRequest> {
        let file_end_line = file_end_line?;
        let required_start = self.required_start_line.max(1);
        if required_start > file_end_line {
            return None;
        }
        let required_end = self
            .required_end_line
            .max(required_start)
            .min(file_end_line);
        let desired_start = self.desired_start_line.max(1).min(file_end_line);
        let desired_end = self.desired_end_line.max(desired_start).min(file_end_line);
        let (start_line, end_line) = anchored_line_window(
            desired_start,
            desired_end,
            required_start,
            required_end,
            self.max_lines,
        );
        Some(ResolvedStoredExcerptRequest {
            file_id: self.file_id,
            start_line,
            end_line,
        })
    }
}

pub(in crate::services) struct AdaptiveExcerptRequest {
    pub(in crate::services) file_id: i64,
    pub(in crate::services) declaration_start: usize,
    pub(in crate::services) declaration_end: usize,
    pub(in crate::services) matched_line: usize,
    pub(in crate::services) token_budget: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedReadTarget {
    pub(super) target_start_line: usize,
    pub(super) target_end_line: Option<usize>,
    pub(super) page_start_line: usize,
    pub(super) page_start_byte: usize,
    pub(super) expected_full_hash: Option<String>,
    pub(super) expected_file_size: Option<usize>,
    pub(super) expected_modified_ns: Option<u128>,
    pub(super) cursor_full: bool,
}

#[derive(Debug)]
pub(super) struct LiveReadRange {
    pub(super) content: String,
    pub(super) page_start_line: usize,
    pub(super) target_bytes: usize,
}

pub(super) struct LiveFileSnapshot {
    /// `None` for bounded reads that stop before EOF; `Some` for full reads.
    pub(super) content_hash: Option<String>,
    pub(super) end_line: usize,
    pub(super) bytes_read: usize,
    pub(super) file_size: usize,
    pub(super) modified_ns: Option<u128>,
}

pub(super) struct LiveReadObservation {
    pub(super) snapshot: LiveFileSnapshot,
    pub(super) range: LiveReadRange,
}

pub(super) struct MaterializedRead {
    pub(super) response: ReadResponse,
    pub(super) baseline_source_tokens: usize,
    pub(super) current_content: String,
    pub(super) current_tokens: usize,
}

#[derive(Debug)]
pub(super) struct ReadCursor {
    pub(super) generation: u64,
    pub(super) target_start_line: usize,
    /// The requested target endpoint. `None` preserves an open-ended read even
    /// when a bounded page stopped before EOF.
    pub(super) target_end_line: Option<usize>,
    pub(super) next_start_line: usize,
    pub(super) next_byte: usize,
    /// Full-file hash for `Full` policy cursors; `None` for `Bounded` cursors.
    pub(super) full_hash: Option<String>,
    pub(super) full: bool,
    pub(super) file_size: usize,
    pub(super) modified_ns: Option<u128>,
    pub(super) path_hash: String,
}
use super::*;
