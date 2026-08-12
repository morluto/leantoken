// Private request, cursor, and materialization state shared by read stages.
#[derive(Debug, Clone)]
pub(in crate::services) enum NewReadTarget {
    Symbol(String),
    Heading {
        name: String,
        occurrence: std::num::NonZeroUsize,
    },
    Lines {
        start: std::num::NonZeroUsize,
        end: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub(super) enum ReadTargetInput {
    New(NewReadTarget),
    Continuation(String),
}

#[derive(Debug, Clone)]
pub(super) enum ReadMode {
    Direct(ReadTargetInput),
}

pub(super) struct ReadInput {
    pub(super) path: String,
    pub(super) mode: ReadMode,
    pub(super) max_tokens: Option<usize>,
    pub(super) expected_hash: Option<String>,
    pub(super) policy: ReadPolicy,
}

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
}

#[derive(Debug)]
pub(super) struct MaterializedRead {
    pub(super) response: ReadResponse,
    pub(super) current_content: String,
}

pub(super) struct ReadBudgetEstimate {
    pub(super) indexed_content: String,
    pub(super) target_source_tokens: usize,
    pub(super) page_start_byte: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct ReadPosition {
    pub(super) target_start_line: usize,
    /// The requested target endpoint. `None` preserves an open-ended read even
    /// when a bounded page stopped before EOF.
    pub(super) target_end_line: Option<usize>,
    pub(super) next_start_line: usize,
    pub(super) next_byte: usize,
}
use super::*;
