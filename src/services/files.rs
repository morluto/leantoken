//! Path discovery: tree, fuzzy find, and glob over the index snapshot.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use globset::Glob;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::index_read::{FilePathRecord, IndexReadSnapshot};
use super::validation::{
    MAX_PATH_BYTES, MAX_PATTERN_BYTES, MAX_QUERY_BYTES, check_cancelled, validate_optional_input,
};
use super::{ServiceCallOptions, Services};
use crate::model::*;
use crate::repository::{slash_path, validate_relative};
use crate::tokens::ResponseBudget;
use crate::{Error, Result};

/// Page size for bounded lean scans over the indexed file table (find / glob fallback).
pub(super) const FILE_LIST_PAGE_SIZE: usize = 1_000;

struct FilePage {
    entries: Vec<FileEntry>,
    next: Option<FileCursor>,
}

#[derive(Clone)]
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

struct FilesInput {
    query: FilesQuery,
    max_results: Option<usize>,
    cursor: Option<(u64, FileCursor)>,
}

enum FilesQuery {
    Tree { root: String, depth: Option<usize> },
    Find { query: String },
    Glob { pattern: String },
}

impl FilesInput {
    fn parse(request: FilesRequest) -> Result<Self> {
        let FilesRequest {
            operation,
            path,
            query,
            pattern,
            max_results,
            cursor,
            depth,
        } = request;
        validate_optional_input(path.as_deref(), "path", MAX_PATH_BYTES)?;
        validate_optional_input(query.as_deref(), "query", MAX_QUERY_BYTES)?;
        validate_optional_input(pattern.as_deref(), "pattern", MAX_PATTERN_BYTES)?;
        let cursor = decode_files_cursor(cursor.as_deref(), &operation)?;
        let query = match operation {
            FileOperation::Tree => FilesQuery::Tree {
                root: normalize_tree_root(path.as_deref())?,
                depth,
            },
            FileOperation::Find => FilesQuery::Find {
                query: query.filter(|value| !value.trim().is_empty()).ok_or(
                    Error::InvalidInput {
                        field: "query",
                        reason: "is required for find",
                    },
                )?,
            },
            FileOperation::Glob => {
                let pattern = pattern.filter(|value| !value.trim().is_empty()).ok_or(
                    Error::InvalidInput {
                        field: "pattern",
                        reason: "is required for glob",
                    },
                )?;
                crate::repository::RepositoryPattern::parse(&pattern)?;
                FilesQuery::Glob { pattern }
            }
        };
        Ok(Self {
            query,
            max_results,
            cursor,
        })
    }

    fn cursor(&self, generation: u64) -> Result<Option<FileCursor>> {
        let Some((cursor_generation, cursor)) = &self.cursor else {
            return Ok(None);
        };
        if *cursor_generation != generation {
            return Err(Error::StaleCursor);
        }
        Ok(Some(cursor.clone()))
    }
}

impl FilesQuery {
    const fn operation(&self) -> FileOperation {
        match self {
            Self::Tree { .. } => FileOperation::Tree,
            Self::Find { .. } => FileOperation::Find,
            Self::Glob { .. } => FileOperation::Glob,
        }
    }
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
    session: &IndexReadSnapshot,
    root: &str,
    depth: Option<usize>,
    cursor: Option<FileCursor>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<FilePage> {
    let max_depth = depth.unwrap_or(usize::MAX);
    let after = cursor_path(cursor)?;
    check_cancelled(cancellation)?;
    let projected =
        session.list_tree_paths(root, max_depth, after.as_deref(), limit.saturating_add(1))?;
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
    session: &IndexReadSnapshot,
    query: &str,
    cursor: Option<FileCursor>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<FilePage> {
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
    for_each_file_path(session, cancellation, |file| {
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
    session: &IndexReadSnapshot,
    pattern: &str,
    cursor: Option<FileCursor>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<FilePage> {
    let normalized_pattern = crate::repository::RepositoryPattern::parse(pattern)?;
    // Validate with globset even when SQL owns matching, so invalid patterns fail
    // the same way as before.
    let matcher = Glob::new(normalized_pattern.as_str())?.compile_matcher();
    let after = cursor_path(cursor)?;
    if let Some((primary, alternate)) = sql_glob_patterns(normalized_pattern.as_str()) {
        check_cancelled(cancellation)?;
        let projected = session.list_glob_paths(
            &primary,
            alternate.as_deref(),
            after.as_deref(),
            limit.saturating_add(1),
        )?;
        let has_more = projected.len() > limit;
        let entries = projected
            .into_iter()
            .take(limit)
            .map(|entry| FileEntry {
                path: entry.path,
                kind: FileEntryKind::File,
                language: entry.language,
                size_bytes: entry.size_bytes,
                score: None,
            })
            .collect::<Vec<_>>();
        let next = has_more
            .then(|| entries.last())
            .flatten()
            .map(|entry| FileCursor::Path {
                operation: PathOperation::Glob,
                path: entry.path.clone(),
            });
        return Ok(FilePage { entries, next });
    }

    // Fallback: brace expansion and other forms SQL GLOB cannot express.
    let capacity = limit.saturating_add(1);
    let mut entries = BTreeMap::new();
    for_each_file_path(session, cancellation, |file| {
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

/// Map a globset-style pattern to one or two SQLite `GLOB` patterns.
///
/// Default globset matching already lets `*` and `?` cross `/`, matching SQLite
/// `GLOB`. The only structural rewrite is collapsing a single `**` form. Brace
/// expansion and multiple `**` segments cannot be expressed as SQL `GLOB`, so
/// those return `None` and the caller keeps the globset scan fallback.
fn sql_glob_patterns(pattern: &str) -> Option<(String, Option<String>)> {
    if pattern.contains(['{', '}']) {
        return None;
    }
    if !pattern.contains("**") {
        return Some((pattern.to_owned(), None));
    }
    if pattern.matches("**").count() != 1 {
        return None;
    }
    if let Some(rest) = pattern.strip_prefix("**/") {
        if rest.contains("**") {
            return None;
        }
        if rest.starts_with('*') {
            return Some((rest.to_owned(), None));
        }
        return Some((rest.to_owned(), Some(format!("*/{rest}"))));
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        if prefix.is_empty() || prefix.contains("**") {
            return None;
        }
        return Some((format!("{prefix}/*"), None));
    }
    if let Some((left, right)) = pattern.split_once("/**/") {
        if left.is_empty() || left.contains("**") || right.contains("**") {
            return None;
        }
        if right.starts_with('*') {
            return Some((format!("{left}/{right}"), None));
        }
        return Some((format!("{left}/{right}"), Some(format!("{left}/*/{right}"))));
    }
    None
}

fn for_each_file_path(
    session: &IndexReadSnapshot,
    cancellation: &CancellationToken,
    mut visitor: impl FnMut(FilePathRecord) -> Result<()>,
) -> Result<()> {
    let mut cursor = None;
    loop {
        check_cancelled(cancellation)?;
        let page = session.list_file_paths(FILE_LIST_PAGE_SIZE, cursor)?;
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

fn decode_files_cursor(
    cursor: Option<&str>,
    operation: &FileOperation,
) -> Result<Option<(u64, FileCursor)>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.len() > MAX_PATH_BYTES.saturating_mul(2).saturating_add(64) {
        return Err(Error::StaleCursor);
    }
    let (cursor_generation, payload) = cursor.split_once(":files:").ok_or(Error::StaleCursor)?;
    let cursor_generation = cursor_generation
        .parse::<u64>()
        .map_err(|_| Error::StaleCursor)?;
    let cursor = match operation {
        FileOperation::Tree | FileOperation::Glob => {
            let (operation_name, operation) = match operation {
                FileOperation::Tree => ("tree:", PathOperation::Tree),
                FileOperation::Glob => ("glob:", PathOperation::Glob),
                FileOperation::Find => return Err(Error::StaleCursor),
            };
            let path = payload
                .strip_prefix(operation_name)
                .ok_or(Error::StaleCursor)?;
            FileCursor::Path {
                operation,
                path: hex_decode(path)?,
            }
        }
        FileOperation::Find => {
            let payload = payload.strip_prefix("find:").ok_or(Error::StaleCursor)?;
            let (score, path) = payload.split_once(':').ok_or(Error::StaleCursor)?;
            FileCursor::Fuzzy {
                score: score.parse().map_err(|_| Error::StaleCursor)?,
                path: hex_decode(path)?,
            }
        }
    };
    Ok(Some((cursor_generation, cursor)))
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

fn files_page(
    input: &FilesInput,
    session: &IndexReadSnapshot,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<(FilePage, FileOperation)> {
    let cursor = input.cursor(session.generation())?;
    let operation = input.query.operation();
    let page = match &input.query {
        FilesQuery::Tree { root, depth } => {
            tree_entries(session, root, *depth, cursor, limit, cancellation)?
        }
        FilesQuery::Find { query } => fuzzy_entries(session, query, cursor, limit, cancellation)?,
        FilesQuery::Glob { pattern } => {
            glob_entries(session, pattern, cursor, limit, cancellation)?
        }
    };
    Ok((page, operation))
}

impl Services {
    /// Discover repository paths.
    pub async fn files(&self, request: FilesRequest) -> Result<FilesResponse> {
        self.files_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Discover repository paths under explicit serialized-response controls.
    pub async fn files_with_options(
        &self,
        request: FilesRequest,
        options: ServiceCallOptions,
    ) -> Result<FilesResponse> {
        self.files_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Discover paths after applying the requested index consistency boundary.
    pub async fn files_with_consistency_cancellable(
        &self,
        request: FilesRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<FilesResponse> {
        self.files_execute(
            request,
            RetrievalExecution::consistent(consistency, ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    /// Discover paths under consistency and serialized-response controls.
    pub async fn files_with_options_consistency_cancellable(
        &self,
        request: FilesRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<FilesResponse> {
        self.files_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    pub async fn files_cancellable(
        &self,
        request: FilesRequest,
        cancellation: CancellationToken,
    ) -> Result<FilesResponse> {
        self.files_execute(
            request,
            RetrievalExecution::direct(ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    /// Discover repository paths without per-entry kind, language, size, or score metadata.
    pub async fn files_paths(&self, request: FilesRequest) -> Result<FilesPathsResponse> {
        self.files_paths_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Discover path-only results under an exact serialized-response bound.
    pub async fn files_paths_with_options(
        &self,
        request: FilesRequest,
        options: ServiceCallOptions,
    ) -> Result<FilesPathsResponse> {
        self.files_paths_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Discover path-only results after applying the requested consistency boundary.
    pub async fn files_paths_with_options_consistency_cancellable(
        &self,
        request: FilesRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<FilesPathsResponse> {
        self.files_paths_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    async fn files_paths_execute(
        &self,
        request: FilesRequest,
        execution: RetrievalExecution,
    ) -> Result<FilesPathsResponse> {
        let operation = TokenAccountingOperation::Files;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, check_cancelled(&cancellation))?;
        let input = self.observe_service_result(operation, FilesInput::parse(request))?;
        let limit = self.observe_service_result(operation, self.result_limit(input.max_results))?;
        if let Some(consistency) = consistency {
            let consistency_result = self
                .apply_consistency_with_initial_deadline(
                    consistency,
                    cancellation.clone(),
                    options.initial_reconciliation_deadline(),
                )
                .await;
            self.observe_service_result(operation, consistency_result)?;
        }
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.files_paths_sync(input, limit, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    async fn files_execute(
        &self,
        request: FilesRequest,
        execution: RetrievalExecution,
    ) -> Result<FilesResponse> {
        let operation = TokenAccountingOperation::Files;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, check_cancelled(&cancellation))?;
        let input = self.observe_service_result(operation, FilesInput::parse(request))?;
        let limit = self.observe_service_result(operation, self.result_limit(input.max_results))?;
        if let Some(consistency) = consistency {
            let consistency_result = self
                .apply_consistency_with_initial_deadline(
                    consistency,
                    cancellation.clone(),
                    options.initial_reconciliation_deadline(),
                )
                .await;
            self.observe_service_result(operation, consistency_result)?;
        }
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.files_sync(input, limit, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn files_sync(
        &self,
        input: FilesInput,
        limit: usize,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<FilesResponse> {
        check_cancelled(cancellation)?;
        let mut response = self.consistent(|session| {
            let (page, operation) = files_page(&input, session, limit, cancellation)?;
            let generation = session.generation();
            let mut response = FilesResponse {
                entries: page.entries,
                meta: self.meta(generation, 0, page.next.map(|next| next.encode(generation))),
            };
            self.fit_files_response(&mut response, operation, generation, options)?;
            Ok(response)
        })?;
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings(TokenAccountingOperation::Files, None, &response.meta);
        Ok(response)
    }

    fn files_paths_sync(
        &self,
        input: FilesInput,
        limit: usize,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<FilesPathsResponse> {
        check_cancelled(cancellation)?;
        let mut response = self.consistent(|session| {
            let (page, operation) = files_page(&input, session, limit, cancellation)?;
            let generation = session.generation();
            let entries = page.entries;
            let mut response = FilesPathsResponse {
                paths: entries.iter().map(|entry| entry.path.clone()).collect(),
                meta: self.meta(generation, 0, page.next.map(|next| next.encode(generation))),
            };
            self.fit_files_paths_response(&mut response, &entries, operation, generation, options)?;
            Ok(response)
        })?;
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings(TokenAccountingOperation::Files, None, &response.meta);
        Ok(response)
    }

    fn fit_files_response(
        &self,
        response: &mut FilesResponse,
        operation: FileOperation,
        generation: u64,
        options: ServiceCallOptions,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }

        let original = response.clone();
        let max_response_tokens = options
            .max_response_tokens()
            .expect("fitting only runs with a response limit");
        let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
        let keep = budget.largest_fitting_prefix(original.entries.len(), |keep| {
            let mut candidate = original.clone();
            candidate.entries.truncate(keep);
            candidate.meta.next_cursor = candidate
                .entries
                .last()
                .map(|entry| files_cursor_for_entry(&operation, entry).encode(generation));
            self.finalized_response_tokens(&candidate)
        })?;
        if let Some(keep) = keep.filter(|keep| *keep > 0) {
            response.entries.truncate(keep);
            response.meta.next_cursor = response
                .entries
                .last()
                .map(|entry| files_cursor_for_entry(&operation, entry).encode(generation));
            return Ok(());
        }

        let minimum = original
            .entries
            .first()
            .map(|entry| {
                let mut minimum = original.clone();
                minimum.entries.truncate(1);
                if original.entries.len() > 1 {
                    minimum.meta.next_cursor =
                        Some(files_cursor_for_entry(&operation, entry).encode(generation));
                }
                minimum
            })
            .unwrap_or(original);
        Err(self.response_budget_error(&minimum, max_response_tokens)?)
    }

    fn fit_files_paths_response(
        &self,
        response: &mut FilesPathsResponse,
        entries: &[FileEntry],
        operation: FileOperation,
        generation: u64,
        options: ServiceCallOptions,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }

        let original = response.clone();
        let max_response_tokens = options
            .max_response_tokens()
            .expect("fitting only runs with a response limit");
        let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
        let keep = budget.largest_fitting_prefix(original.paths.len(), |keep| {
            let mut candidate = original.clone();
            candidate.paths.truncate(keep);
            candidate.meta.next_cursor = entries
                .get(keep.saturating_sub(1))
                .map(|entry| files_cursor_for_entry(&operation, entry).encode(generation));
            self.finalized_response_tokens(&candidate)
        })?;
        if let Some(keep) = keep.filter(|keep| *keep > 0) {
            response.paths.truncate(keep);
            response.meta.next_cursor = entries
                .get(keep - 1)
                .map(|entry| files_cursor_for_entry(&operation, entry).encode(generation));
            return Ok(());
        }

        let minimum = entries
            .first()
            .map(|entry| {
                let mut minimum = original.clone();
                minimum.paths.truncate(1);
                if original.paths.len() > 1 {
                    minimum.meta.next_cursor =
                        Some(files_cursor_for_entry(&operation, entry).encode(generation));
                }
                minimum
            })
            .unwrap_or(original);
        Err(self.response_budget_error(&minimum, max_response_tokens)?)
    }
}

fn files_cursor_for_entry(operation: &FileOperation, entry: &FileEntry) -> FileCursor {
    match operation {
        FileOperation::Tree => FileCursor::Path {
            operation: PathOperation::Tree,
            path: entry.path.clone(),
        },
        FileOperation::Glob => FileCursor::Path {
            operation: PathOperation::Glob,
            path: entry.path.clone(),
        },
        FileOperation::Find => FileCursor::Fuzzy {
            score: entry
                .score
                .expect("fuzzy results retain their cursor score") as u32,
            path: entry.path.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::sql_glob_patterns;

    #[test]
    fn sql_glob_patterns_map_common_double_star_forms() {
        assert_eq!(sql_glob_patterns("*.rs"), Some(("*.rs".into(), None)));
        assert_eq!(sql_glob_patterns("**/*.rs"), Some(("*.rs".into(), None)));
        assert_eq!(
            sql_glob_patterns("src/**/*.rs"),
            Some(("src/*.rs".into(), None))
        );
        assert_eq!(sql_glob_patterns("src/**"), Some(("src/*".into(), None)));
        assert_eq!(
            sql_glob_patterns("**/lib.rs"),
            Some(("lib.rs".into(), Some("*/lib.rs".into())))
        );
        assert_eq!(
            sql_glob_patterns("src/**/lib.rs"),
            Some(("src/lib.rs".into(), Some("src/*/lib.rs".into())))
        );
    }

    #[test]
    fn sql_glob_patterns_fall_back_for_unexpressible_forms() {
        assert_eq!(sql_glob_patterns("{a,b}.rs"), None);
        assert_eq!(sql_glob_patterns("a/**/b/**/c"), None);
        assert_eq!(sql_glob_patterns("**"), None);
    }
}
