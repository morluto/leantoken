//! Bounded live reads, outlines, and index-backed excerpts.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use tokio_util::sync::CancellationToken;

use super::Services;
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input,
    validate_optional_input,
};
use crate::model::*;
use crate::repository::{resolve_existing, validate_relative};
use crate::storage::ReadSession;
use crate::text::{anchored_line_window, hash};
use crate::{Error, Result};

const MIN_CONTEXT_RANGE_LINES: usize = 12;
const MAX_CONTEXT_RANGE_LINES: usize = 128;

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

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug, Clone, Copy)]
struct ResolvedReadTarget {
    start_line: usize,
    end_line: Option<usize>,
}

#[derive(Debug)]
struct LiveReadRange {
    content: String,
    start_line: usize,
    end_line: usize,
}

fn assemble_stored_excerpt(
    request: ResolvedStoredExcerptRequest,
    selected: &[crate::storage::ChunkRecord],
) -> Option<StoredExcerpt> {
    let first_chunk = selected.first()?;
    let base_line = first_chunk.start_line;
    let mut combined = String::new();
    for chunk in selected {
        combined.push_str(&chunk.content);
    }
    let local_start = request.start_line.saturating_sub(base_line) + 1;
    let local_end = request.end_line.saturating_sub(base_line) + 1;
    Some(StoredExcerpt {
        content: crate::text::excerpt(&combined, local_start, local_end),
        start_line: request.start_line,
        end_line: request.end_line,
    })
}

fn storage_symbol(symbol: crate::storage::SymbolRecord) -> Symbol {
    Symbol {
        name: symbol.name,
        kind: symbol.kind,
        parent: symbol.parent,
        signature: symbol.signature,
        start_line: symbol.start_line,
        end_line: symbol.end_line,
        start_byte: symbol.start_byte,
        end_byte: symbol.end_byte,
    }
}

fn validate_outline_input(request: &OutlineRequest) -> Result<()> {
    if request.paths.is_empty() {
        return Err(Error::InvalidInput {
            field: "paths",
            reason: "must contain at least one path",
        });
    }
    if request.paths.len() > MAX_INPUT_ITEMS {
        return Err(Error::LimitExceeded);
    }
    for path in &request.paths {
        validate_input(path, "path", MAX_PATH_BYTES)?;
        validate_relative(path)?;
    }
    validate_optional_input(
        request.symbol_name.as_deref(),
        "symbol name",
        MAX_PATTERN_BYTES,
    )?;
    validate_optional_input(
        request.symbol_kind.as_deref(),
        "symbol kind",
        MAX_PATTERN_BYTES,
    )?;
    Ok(())
}

fn validate_read_input(request: &ReadRequest) -> Result<()> {
    validate_input(&request.path, "path", MAX_PATH_BYTES)?;
    if request.symbol.as_deref().is_some_and(str::is_empty) {
        return Err(Error::InvalidInput {
            field: "symbol",
            reason: "must not be empty",
        });
    }
    validate_optional_input(request.symbol.as_deref(), "symbol", MAX_PATTERN_BYTES)?;
    validate_optional_input(request.expected_hash.as_deref(), "expected hash", 128)?;
    validate_relative(&request.path)?;
    if request.symbol.is_some() && (request.start_line.is_some() || request.end_line.is_some()) {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a symbol or line range, not both",
        });
    }
    if request.symbol.is_none() {
        let start_line = request.start_line.unwrap_or(1);
        if start_line == 0
            || request
                .end_line
                .is_some_and(|end_line| end_line < start_line)
        {
            return Err(invalid_line_range());
        }
    }
    Ok(())
}

impl Services {
    /// Return bounded structural outlines for indexed files.
    pub async fn outline(&self, request: OutlineRequest) -> Result<OutlineResponse> {
        self.outline_cancellable(request, CancellationToken::new())
            .await
    }

    /// Outline files after applying the requested index consistency boundary.
    pub async fn outline_with_consistency_cancellable(
        &self,
        request: OutlineRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<OutlineResponse> {
        validate_outline_input(&request)?;
        self.result_limit(request.max_results)?;
        self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.outline_cancellable(request, cancellation).await
    }

    pub async fn outline_cancellable(
        &self,
        request: OutlineRequest,
        cancellation: CancellationToken,
    ) -> Result<OutlineResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.outline_sync(request, &cancellation)).await?
    }

    /// Read a bounded live source range and report index staleness.
    pub async fn read(&self, request: ReadRequest) -> Result<ReadResponse> {
        self.read_cancellable(request, CancellationToken::new())
            .await
    }

    /// Read source after applying the requested index consistency boundary.
    pub async fn read_with_consistency_cancellable(
        &self,
        request: ReadRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        validate_read_input(&request)?;
        self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        self.apply_consistency(consistency, cancellation.clone())
            .await?;
        self.read_cancellable(request, cancellation).await
    }

    pub async fn read_cancellable(
        &self,
        request: ReadRequest,
        cancellation: CancellationToken,
    ) -> Result<ReadResponse> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.read_sync(request, &cancellation)).await?
    }

    fn outline_sync(
        &self,
        request: OutlineRequest,
        cancellation: &CancellationToken,
    ) -> Result<OutlineResponse> {
        check_cancelled(cancellation)?;
        validate_outline_input(&request)?;
        let limit = self.result_limit(request.max_results)?;
        let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        self.consistent(|session, generation| {
            let mut remaining = limit;
            let mut emitted_tokens = 0usize;
            let mut files = Vec::new();
            for path in &request.paths {
                check_cancelled(cancellation)?;
                let file = session
                    .find_file(path)?
                    .ok_or_else(|| Error::NotIndexed(path.clone()))?;
                let mut symbols = session
                    .get_symbols_for_file_filtered(
                        file.id,
                        request.symbol_name.as_deref(),
                        request.symbol_kind.as_deref(),
                        remaining.max(1),
                    )?
                    .into_iter()
                    .map(storage_symbol)
                    .collect::<Vec<_>>();
                symbols.retain(|symbol| {
                    let cost = symbol
                        .signature
                        .as_deref()
                        .map_or(1, |value| self.config.tokenizer.count(value));
                    if remaining == 0 || emitted_tokens.saturating_add(cost) > token_limit {
                        false
                    } else {
                        remaining -= 1;
                        emitted_tokens += cost;
                        true
                    }
                });
                let mut imports = session
                    .get_imports_for_file(file.id, limit)?
                    .into_iter()
                    .map(|import| Import {
                        raw_target: import.raw_target,
                        resolved_path: import.resolved_path,
                        line: import.line,
                    })
                    .collect::<Vec<_>>();
                imports.retain(|import| {
                    let cost = self.config.tokenizer.count(&import.raw_target)
                        + import
                            .resolved_path
                            .as_deref()
                            .map_or(0, |value| self.config.tokenizer.count(value));
                    if remaining == 0 || emitted_tokens.saturating_add(cost) > token_limit {
                        false
                    } else {
                        remaining -= 1;
                        emitted_tokens += cost;
                        true
                    }
                });
                files.push(OutlineFile {
                    path: file.path,
                    language: file.language.clone(),
                    structurally_complete: file.structurally_complete,
                    symbols,
                    imports,
                });
                if remaining == 0 {
                    break;
                }
            }
            Ok(OutlineResponse {
                files,
                meta: self.meta(generation, emitted_tokens, None),
            })
        })
    }

    fn read_sync(
        &self,
        request: ReadRequest,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        validate_read_input(&request)?;
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        self.consistent(|session, generation| {
            check_cancelled(cancellation)?;
            self.read_at_generation(session, &request, generation, max_tokens)
        })
    }

    fn read_at_generation(
        &self,
        session: &ReadSession,
        request: &ReadRequest,
        generation: u64,
        max_tokens: usize,
    ) -> Result<ReadResponse> {
        let indexed = session
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let path = resolve_existing(&self.config.root, &request.path)?;
        let target = resolve_read_target(session, indexed.id, request)?;

        // Stream the file through a BufReader for the full-file hash so the
        // entire file does not need to be held in memory simultaneously. The
        // content range is extracted by a bounded line-oriented reader.
        let file = fs::File::open(&path)?;
        let full_hash = stream_hash(&file)?;
        let range = read_live_range(&file, target)?;
        let start_line = range.start_line;
        let (content, emitted_tokens) = self.config.tokenizer.truncate(&range.content, max_tokens);
        let returned_lines = content
            .lines()
            .count()
            .max(usize::from(!content.is_empty()));
        let end_line = if returned_lines == 0 {
            start_line
        } else {
            (start_line + returned_lines - 1).min(range.end_line)
        };
        let content_hash = hash(content);
        let index_stale = indexed.content_hash != full_hash;
        let indexed_hash = Some(indexed.content_hash);
        let not_modified = request.expected_hash.as_deref() == Some(content_hash.as_str());

        Ok(ReadResponse {
            path: request.path.clone(),
            status: if not_modified {
                ReadStatus::NotModified
            } else {
                ReadStatus::Content
            },
            start_line,
            end_line,
            content: (!not_modified).then(|| content.to_string()),
            content_hash,
            indexed_hash,
            index_stale,
            meta: self.meta(
                generation,
                if not_modified { 0 } else { emitted_tokens },
                None,
            ),
        })
    }
}

fn resolve_read_target(
    session: &ReadSession,
    file_id: i64,
    request: &ReadRequest,
) -> Result<ResolvedReadTarget> {
    let target = if let Some(symbol_name) = &request.symbol {
        let symbol = session
            .find_symbol(file_id, symbol_name)?
            .ok_or_else(|| Error::NotIndexed(format!("{}::{symbol_name}", request.path)))?;
        ResolvedReadTarget {
            start_line: symbol.start_line,
            end_line: Some(symbol.end_line),
        }
    } else {
        ResolvedReadTarget {
            start_line: request.start_line.unwrap_or(1),
            end_line: request.end_line,
        }
    };

    if target.start_line == 0
        || target
            .end_line
            .is_some_and(|end_line| end_line < target.start_line)
    {
        return Err(invalid_line_range());
    }
    Ok(target)
}

/// Stream a file through a BufReader and compute the content hash without
/// loading the entire file into memory.
fn stream_hash(file: &File) -> Result<String> {
    let mut reader = BufReader::new(file.try_clone()?);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN].to_string())
}

/// Read a resolved range without changing its original line terminators.
fn read_live_range(file: &File, target: ResolvedReadTarget) -> Result<LiveReadRange> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut selected = Vec::new();
    let mut line = Vec::new();
    let mut current_line = 0usize;
    let mut selected_end = None;

    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        current_line += 1;
        if current_line >= target.start_line
            && target
                .end_line
                .is_none_or(|end_line| current_line <= end_line)
        {
            selected.extend_from_slice(&line);
            selected_end = Some(current_line);
        }
        if target.end_line == Some(current_line) {
            break;
        }
    }

    let logical_line_count = current_line.max(1);
    if target.start_line > logical_line_count {
        return Err(invalid_line_range());
    }
    let content = String::from_utf8(selected).map_err(|_| Error::InvalidInput {
        field: "path",
        reason: "must identify UTF-8 text",
    })?;
    Ok(LiveReadRange {
        content,
        start_line: target.start_line,
        end_line: selected_end.unwrap_or(target.start_line),
    })
}

fn invalid_line_range() -> Error {
    Error::InvalidInput {
        field: "line range",
        reason: "must be ordered and within the requested file",
    }
}

#[cfg(test)]
impl Services {
    pub(super) fn stored_excerpt(
        &self,
        session: &ReadSession,
        file_id: i64,
        start_line: usize,
        end_line: usize,
        context: usize,
        max_lines: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let request = StoredExcerptRequest {
            file_id,
            desired_start_line: start_line.saturating_sub(context).max(1),
            desired_end_line: end_line.saturating_add(context),
            required_start_line: start_line,
            required_end_line: end_line,
            max_lines,
        };
        Ok(self
            .stored_excerpts(session, &[request])?
            .into_iter()
            .next()
            .flatten())
    }
}

impl Services {
    pub(super) fn stored_excerpts(
        &self,
        session: &ReadSession,
        requests: &[StoredExcerptRequest],
    ) -> Result<Vec<Option<StoredExcerpt>>> {
        let file_ids = requests
            .iter()
            .map(|request| request.file_id)
            .collect::<Vec<_>>();
        let file_end_lines = session.file_end_lines_batch(&file_ids)?;
        let mut resolved = Vec::new();
        let mut ranges = Vec::new();
        for (index, (request, file_end_line)) in requests.iter().zip(file_end_lines).enumerate() {
            let Some(request) = request.resolve(file_end_line) else {
                continue;
            };
            ranges.push((request.file_id, request.start_line, request.end_line));
            resolved.push((index, request));
        }
        let chunks = session.get_chunks_overlapping_batch(&ranges)?;
        let mut excerpts = vec![None; requests.len()];
        for ((index, request), chunks) in resolved.into_iter().zip(chunks) {
            excerpts[index] = assemble_stored_excerpt(request, &chunks);
        }
        Ok(excerpts)
    }

    #[cfg(test)]
    pub(super) fn adaptive_context_excerpt(
        &self,
        session: &ReadSession,
        file_id: i64,
        declaration_start: usize,
        declaration_end: usize,
        matched_line: usize,
        token_budget: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let Some(full) =
            self.stored_excerpt(session, file_id, declaration_start, declaration_end, 0, 0)?
        else {
            return Ok(None);
        };
        let full_tokens = self.config.tokenizer.count(&full.content).max(1);
        if full_tokens <= token_budget {
            return Ok(Some(full));
        }

        let declaration_lines = declaration_end
            .saturating_sub(declaration_start)
            .saturating_add(1);
        let proportional_lines = declaration_lines
            .saturating_mul(token_budget)
            .saturating_div(full_tokens)
            .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
            .min(declaration_lines);
        let before = proportional_lines / 3;
        let mut start = matched_line.saturating_sub(before).max(declaration_start);
        let mut end = start
            .saturating_add(proportional_lines.saturating_sub(1))
            .min(declaration_end);
        if end.saturating_sub(start).saturating_add(1) < proportional_lines {
            start = end
                .saturating_add(1)
                .saturating_sub(proportional_lines)
                .max(declaration_start);
        }
        end = start
            .saturating_add(proportional_lines.saturating_sub(1))
            .min(declaration_end);
        self.stored_excerpt(session, file_id, start, end, 0, 0)
    }

    pub(super) fn adaptive_context_excerpts(
        &self,
        session: &ReadSession,
        requests: &[AdaptiveExcerptRequest],
    ) -> Result<Vec<Option<StoredExcerpt>>> {
        let full_requests = requests
            .iter()
            .map(|request| StoredExcerptRequest {
                file_id: request.file_id,
                desired_start_line: request.declaration_start,
                desired_end_line: request.declaration_end,
                required_start_line: request.matched_line,
                required_end_line: request.matched_line,
                max_lines: 0,
            })
            .collect::<Vec<_>>();
        let mut excerpts = self.stored_excerpts(session, &full_requests)?;
        let mut narrowed_indices = Vec::new();
        let mut narrowed_requests = Vec::new();
        for (index, (request, excerpt)) in requests.iter().zip(&excerpts).enumerate() {
            let Some(excerpt) = excerpt else {
                continue;
            };
            let full_tokens = self.config.tokenizer.count(&excerpt.content).max(1);
            if full_tokens <= request.token_budget {
                continue;
            }
            let declaration_lines = request
                .declaration_end
                .saturating_sub(request.declaration_start)
                .saturating_add(1);
            let proportional_lines = declaration_lines
                .saturating_mul(request.token_budget)
                .saturating_div(full_tokens)
                .clamp(MIN_CONTEXT_RANGE_LINES, MAX_CONTEXT_RANGE_LINES)
                .min(declaration_lines);
            let before = proportional_lines / 3;
            let mut start = request
                .matched_line
                .saturating_sub(before)
                .max(request.declaration_start);
            let mut end = start
                .saturating_add(proportional_lines.saturating_sub(1))
                .min(request.declaration_end);
            if end.saturating_sub(start).saturating_add(1) < proportional_lines {
                start = end
                    .saturating_add(1)
                    .saturating_sub(proportional_lines)
                    .max(request.declaration_start);
            }
            end = start
                .saturating_add(proportional_lines.saturating_sub(1))
                .min(request.declaration_end);
            narrowed_indices.push(index);
            narrowed_requests.push(StoredExcerptRequest {
                file_id: request.file_id,
                desired_start_line: start,
                desired_end_line: end,
                required_start_line: request.matched_line,
                required_end_line: request.matched_line,
                max_lines: 0,
            });
        }
        let narrowed = self.stored_excerpts(session, &narrowed_requests)?;
        for (index, excerpt) in narrowed_indices.into_iter().zip(narrowed) {
            excerpts[index] = excerpt;
        }
        Ok(excerpts)
    }
}
