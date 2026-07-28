// Private request, cursor, and materialization state shared by read stages.
#[derive(Clone)]
pub(super) struct StoredExcerpt {
    pub(super) content: String,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
}

pub(super) struct StoredExcerptRequest {
    pub file_id: i64,
    pub desired_start_line: usize,
    pub desired_end_line: usize,
    pub required_start_line: usize,
    pub required_end_line: usize,
    pub max_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ResolvedStoredExcerptRequest {
    file_id: i64,
    start_line: usize,
    end_line: usize,
}

impl StoredExcerptRequest {
    fn resolve(&self, file_end_line: Option<usize>) -> Option<ResolvedStoredExcerptRequest> {
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

pub(super) struct AdaptiveExcerptRequest {
    pub file_id: i64,
    pub declaration_start: usize,
    pub declaration_end: usize,
    pub matched_line: usize,
    pub token_budget: usize,
}

#[derive(Debug, Clone)]
struct ResolvedReadTarget {
    target_start_line: usize,
    target_end_line: Option<usize>,
    page_start_line: usize,
    page_start_byte: usize,
    expected_full_hash: Option<String>,
}

#[derive(Debug)]
struct LiveReadRange {
    content: String,
    page_start_line: usize,
    target_bytes: usize,
}

struct LiveFileSnapshot {
    content_hash: String,
    end_line: usize,
}

struct LiveReadObservation {
    snapshot: LiveFileSnapshot,
    range: LiveReadRange,
}

struct MaterializedRead {
    response: ReadResponse,
    baseline_source_tokens: usize,
    current_content: String,
    current_tokens: usize,
}

#[derive(Debug)]
struct ReadCursor {
    generation: u64,
    target_start_line: usize,
    target_end_line: usize,
    next_start_line: usize,
    next_byte: usize,
    full_hash: String,
    path_hash: String,
}
