use super::*;
use crate::services::cursor::request_digest;

#[derive(serde::Serialize)]
struct WorktreeCursorRequest<'a> {
    path: &'a str,
    policy: ReadPolicy,
}

pub(super) fn open_read_cursor(
    generation: &RepositoryGeneration,
    token: &str,
    path: &str,
    policy: ReadPolicy,
) -> Result<ReadCursor> {
    generation.open_cursor(
        token,
        "read_worktree",
        &request_digest(&WorktreeCursorRequest { path, policy })?,
    )
}

pub(super) fn seal_read_cursor(
    generation: &RepositoryGeneration,
    path: &str,
    policy: ReadPolicy,
    position: ReadCursor,
) -> Result<String> {
    generation.seal_cursor(
        "read_worktree",
        &request_digest(&WorktreeCursorRequest { path, policy })?,
        position,
    )
}

pub(super) fn returned_end_line(start_line: usize, content: &str) -> usize {
    let newline_count = content.bytes().filter(|byte| *byte == b'\n').count();
    start_line
        .saturating_add(newline_count)
        .saturating_sub(usize::from(content.ends_with('\n') && newline_count > 0))
}
