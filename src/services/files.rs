//! Path discovery: tree, fuzzy find, and glob over the index snapshot.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use globset::Glob;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use tokio_util::sync::CancellationToken;

use super::Services;
use super::validation::{
    MAX_PATH_BYTES, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, check_cancelled, validate_optional_input,
};
use crate::model::*;
use crate::repository::{slash_path, validate_relative};
use crate::storage::{FileRecord, ReadSession};
use crate::{Error, Result};

/// Page size for bounded scans over the indexed file table.
pub(super) const FILE_LIST_PAGE_SIZE: usize = 1_000;

struct FilePage {
    entries: Vec<FileEntry>,
    next: Option<FileCursor>,
}

enum FileCursor {
    Path {
        operation: PathOperation,
        path: String,
    },
    Fuzzy {
        score: u32,
        path: String,
    },
}

#[derive(Clone, Copy)]
enum PathOperation {
    Tree,
    Glob,
}

impl FileCursor {
    fn encode(self, generation: u64) -> String {
        match self {
            Self::Path { operation, path } => {
                let operation = match operation {
                    PathOperation::Tree => "tree",
                    PathOperation::Glob => "glob",
                };
                format!("{generation}:files:{operation}:{}", hex_encode(&path))
            }
            Self::Fuzzy { score, path } => {
                format!("{generation}:files:find:{score}:{}", hex_encode(&path))
            }
        }
    }
}

fn tree_entries(
    session: &ReadSession,
    root: Option<&str>,
    depth: Option<usize>,
    cursor: Option<FileCursor>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<FilePage> {
    let root = normalize_tree_root(root)?;
    let max_depth = depth.unwrap_or(usize::MAX);
    let after = cursor_path(cursor)?;
    check_cancelled(cancellation)?;
    let projected =
        session.list_tree_paths(&root, max_depth, after.as_deref(), limit.saturating_add(1))?;
    let has_more = projected.len() > limit;
    let entries = projected
        .into_iter()
        .take(limit)
        .map(|entry| FileEntry {
            path: entry.path,
            kind: if entry.is_directory {
                FileEntryKind::Directory
            } else {
                FileEntryKind::File
            },
            language: entry.language,
            size_bytes: entry.size_bytes,
            score: None,
        })
        .collect::<Vec<_>>();
    let next = has_more
        .then(|| entries.last())
        .flatten()
        .map(|entry| FileCursor::Path {
            operation: PathOperation::Tree,
            path: entry.path.clone(),
        });
    Ok(FilePage { entries, next })
}

fn normalize_tree_root(root: Option<&str>) -> Result<String> {
    let Some(root) = root else {
        return Ok(String::new());
    };
    if root.is_empty() {
        return Ok(String::new());
    }
    Ok(slash_path(&validate_relative(root)?))
}

fn fuzzy_entries(
    session: &ReadSession,
    query: Option<&str>,
    cursor: Option<FileCursor>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<FilePage> {
    let query = query
        .filter(|value| !value.is_empty())
        .ok_or(Error::InvalidInput {
            field: "query",
            reason: "is required for find",
        })?;
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
    let mut unicode_buf = Vec::new();
    let after = match cursor {
        Some(FileCursor::Fuzzy { score, path }) => Some((Reverse(score), path)),
        Some(FileCursor::Path { .. }) => return Err(Error::StaleCursor),
        None => None,
    };
    let capacity = limit.saturating_add(1);
    let mut entries = BTreeMap::new();
    for_each_file(session, cancellation, |file| {
        let Some(score) = pattern.score(Utf32Str::new(&file.path, &mut unicode_buf), &mut matcher)
        else {
            return Ok(());
        };
        let key = (Reverse(score), file.path.clone());
        if after.as_ref().is_none_or(|after| key > *after) {
            entries.insert(
                key,
                FileEntry {
                    path: file.path,
                    kind: FileEntryKind::File,
                    language: file.language,
                    size_bytes: Some(file.size_bytes),
                    score: Some(f64::from(score)),
                },
            );
            if entries.len() > capacity {
                entries.pop_last();
            }
        }
        Ok(())
    })?;
    let has_more = entries.len() > limit;
    let selected = entries.into_iter().take(limit).collect::<Vec<_>>();
    let next = has_more
        .then(|| selected.last())
        .flatten()
        .map(|((Reverse(score), path), _)| FileCursor::Fuzzy {
            score: *score,
            path: path.clone(),
        });
    Ok(FilePage {
        entries: selected.into_iter().map(|(_, entry)| entry).collect(),
        next,
    })
}

fn glob_entries(
    session: &ReadSession,
    pattern: Option<&str>,
    cursor: Option<FileCursor>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<FilePage> {
    let pattern = pattern
        .filter(|value| !value.is_empty())
        .ok_or(Error::InvalidInput {
            field: "pattern",
            reason: "is required for glob",
        })?;
    let matcher = Glob::new(pattern)?.compile_matcher();
    let after = cursor_path(cursor)?;
    let capacity = limit.saturating_add(1);
    let mut entries = BTreeMap::new();
    for_each_file(session, cancellation, |file| {
        if matcher.is_match(&file.path) {
            retain_path_entry(
                &mut entries,
                FileEntry {
                    path: file.path,
                    kind: FileEntryKind::File,
                    language: file.language,
                    size_bytes: Some(file.size_bytes),
                    score: None,
                },
                after.as_deref(),
                capacity,
            );
        }
        Ok(())
    })?;
    Ok(finish_path_page(entries, limit, PathOperation::Glob))
}

fn validate_files_input(request: &FilesRequest) -> Result<()> {
    validate_optional_input(request.path.as_deref(), "path", MAX_PATH_BYTES)?;
    validate_optional_input(request.query.as_deref(), "query", MAX_QUERY_BYTES)?;
    validate_optional_input(request.pattern.as_deref(), "pattern", MAX_PATTERN_BYTES)?;
    match request.operation {
        FileOperation::Tree => {
            normalize_tree_root(request.path.as_deref())?;
        }
        FileOperation::Find => {
            request
                .query
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or(Error::InvalidInput {
                    field: "query",
                    reason: "is required for find",
                })?;
        }
        FileOperation::Glob => {
            let pattern = request
                .pattern
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or(Error::InvalidInput {
                    field: "pattern",
                    reason: "is required for glob",
                })?;
            Glob::new(pattern)?;
        }
    }
    Ok(())
}

fn for_each_file(
    session: &ReadSession,
    cancellation: &CancellationToken,
    mut visitor: impl FnMut(FileRecord) -> Result<()>,
) -> Result<()> {
    let mut cursor = None;
    loop {
        check_cancelled(cancellation)?;
        let page = session.list_files(FILE_LIST_PAGE_SIZE, cursor)?;
        if page.is_empty() {
            return Ok(());
        }
        cursor = page.last().map(|file| file.id);
        for file in page {
            check_cancelled(cancellation)?;
            visitor(file)?;
        }
    }
}

fn cursor_path(cursor: Option<FileCursor>) -> Result<Option<String>> {
    match cursor {
        Some(FileCursor::Path { path, .. }) => Ok(Some(path)),
        Some(FileCursor::Fuzzy { .. }) => Err(Error::StaleCursor),
        None => Ok(None),
    }
}

fn retain_path_entry(
    entries: &mut BTreeMap<String, FileEntry>,
    entry: FileEntry,
    after: Option<&str>,
    capacity: usize,
) {
    if after.is_some_and(|after| entry.path.as_str() <= after) {
        return;
    }
    entries.entry(entry.path.clone()).or_insert(entry);
    if entries.len() > capacity {
        entries.pop_last();
    }
}

fn finish_path_page(
    entries: BTreeMap<String, FileEntry>,
    limit: usize,
    operation: PathOperation,
) -> FilePage {
    let has_more = entries.len() > limit;
    let entries = entries.into_values().take(limit).collect::<Vec<_>>();
    let next = has_more
        .then(|| entries.last())
        .flatten()
        .map(|entry| FileCursor::Path {
            operation,
            path: entry.path.clone(),
        });
    FilePage { entries, next }
}

fn parse_files_cursor(
    cursor: Option<&str>,
    generation: u64,
    operation: &FileOperation,
) -> Result<Option<FileCursor>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.len() > MAX_PATH_BYTES.saturating_mul(2).saturating_add(64) {
        return Err(Error::StaleCursor);
    }
    let prefix = format!("{generation}:files:");
    let payload = cursor.strip_prefix(&prefix).ok_or(Error::StaleCursor)?;
    match operation {
        FileOperation::Tree | FileOperation::Glob => {
            let (operation_name, operation) = match operation {
                FileOperation::Tree => ("tree:", PathOperation::Tree),
                FileOperation::Glob => ("glob:", PathOperation::Glob),
                FileOperation::Find => return Err(Error::StaleCursor),
            };
            let path = payload
                .strip_prefix(operation_name)
                .ok_or(Error::StaleCursor)?;
            Ok(Some(FileCursor::Path {
                operation,
                path: hex_decode(path)?,
            }))
        }
        FileOperation::Find => {
            let payload = payload.strip_prefix("find:").ok_or(Error::StaleCursor)?;
            let (score, path) = payload.split_once(':').ok_or(Error::StaleCursor)?;
            Ok(Some(FileCursor::Fuzzy {
                score: score.parse().map_err(|_| Error::StaleCursor)?,
                path: hex_decode(path)?,
            }))
        }
    }
}

fn hex_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode(value: &str) -> Result<String> {
    if !value.len().is_multiple_of(2) {
        return Err(Error::StaleCursor);
    }
    let decoded = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect::<Result<Vec<_>>>()?;
    String::from_utf8(decoded).map_err(|_| Error::StaleCursor)
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(Error::StaleCursor),
    }
}

impl Services {
    /// Discover repository paths.
    pub async fn files(&self, request: FilesRequest) -> Result<FilesResponse> {
        self.files_cancellable(request, CancellationToken::new())
            .await
    }

    /// Discover paths after applying the requested index consistency boundary.
    pub async fn files_with_consistency_cancellable(
        &self,
        request: FilesRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<FilesResponse> {
        validate_files_input(&request)?;
        self.result_limit(request.max_results)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.files_cancellable(request, cancellation).await
    }

    pub async fn files_cancellable(
        &self,
        request: FilesRequest,
        cancellation: CancellationToken,
    ) -> Result<FilesResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.files_sync(request, &cancellation)).await?
    }

    fn files_sync(
        &self,
        request: FilesRequest,
        cancellation: &CancellationToken,
    ) -> Result<FilesResponse> {
        check_cancelled(cancellation)?;
        validate_files_input(&request)?;
        let limit = self.result_limit(request.max_results)?;
        self.consistent(|session, generation| {
            let cursor =
                parse_files_cursor(request.cursor.as_deref(), generation, &request.operation)?;
            let page = match request.operation {
                FileOperation::Tree => tree_entries(
                    session,
                    request.path.as_deref(),
                    request.depth,
                    cursor,
                    limit,
                    cancellation,
                )?,
                FileOperation::Find => fuzzy_entries(
                    session,
                    request.query.as_deref(),
                    cursor,
                    limit,
                    cancellation,
                )?,
                FileOperation::Glob => glob_entries(
                    session,
                    request.pattern.as_deref(),
                    cursor,
                    limit,
                    cancellation,
                )?,
            };
            Ok(FilesResponse {
                entries: page.entries,
                meta: self.meta(generation, 0, page.next.map(|next| next.encode(generation))),
            })
        })
    }
}
