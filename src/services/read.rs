//! Bounded live reads, outlines, and index-backed excerpts.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use tokio_util::sync::CancellationToken;

use super::Services;
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input,
    validate_optional_input,
};
use crate::model::*;
use crate::repository::{normalize_relative, resolve_existing, validate_relative};
use crate::storage::ReadSession;
use crate::text::{anchored_line_window, hash};
use crate::{Error, Result};

const MIN_CONTEXT_RANGE_LINES: usize = 12;
const MAX_CONTEXT_RANGE_LINES: usize = 128;
// Re-tokenize bounded candidate windows instead of guessing a byte/token ratio.
// The hard cap prevents pathological low-token inputs from growing forever.
const LIVE_READ_TOKEN_CHECK_BYTES: usize = 64 * 1024;
const MAX_LIVE_READ_BYTES: usize = 8 * 1024 * 1024;

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

impl ReadCursor {
    fn encode(&self) -> String {
        format!(
            "{}:read:{}:{}:{}:{}:{}:{}",
            self.generation,
            self.target_start_line,
            self.target_end_line,
            self.next_start_line,
            self.next_byte,
            self.full_hash,
            self.path_hash,
        )
    }
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

fn decode_outline_cursor(cursor: &str) -> Result<(u64, usize, String)> {
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [generation, kind, offset, query_hash] = fields.as_slice() else {
        return Err(Error::StaleCursor);
    };
    if *kind != "outline" || query_hash.len() != 16 || !query_hash.bytes().all(is_lower_hex) {
        return Err(Error::StaleCursor);
    }
    Ok((
        generation.parse().map_err(|_| Error::StaleCursor)?,
        offset.parse().map_err(|_| Error::StaleCursor)?,
        (*query_hash).into(),
    ))
}

fn outline_query_hash(request: &OutlineRequest) -> String {
    fn update_field(hasher: &mut blake3::Hasher, value: &str) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(&(request.paths.len() as u64).to_le_bytes());
    for path in &request.paths {
        update_field(&mut hasher, path);
    }
    for value in [&request.symbol_name, &request.symbol_kind] {
        match value {
            Some(value) => {
                hasher.update(&[1]);
                update_field(&mut hasher, value);
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
    hasher.finalize().to_hex()[..16].to_string()
}

fn parse_outline_cursor(
    cursor: Option<&str>,
    generation: u64,
    request: &OutlineRequest,
) -> Result<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let (cursor_generation, offset, query_hash) = decode_outline_cursor(cursor)?;
    if cursor_generation != generation || query_hash != outline_query_hash(request) {
        return Err(Error::StaleCursor);
    }
    Ok(offset)
}

fn make_outline_cursor(generation: u64, offset: usize, request: &OutlineRequest) -> String {
    format!(
        "{generation}:outline:{offset}:{}",
        outline_query_hash(request)
    )
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
    validate_optional_input(request.cursor.as_deref(), "cursor", 256)?;
    if let Some(cursor) = request.cursor.as_deref() {
        decode_outline_cursor(cursor)?;
    }
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
    if request
        .heading
        .as_deref()
        .is_some_and(|heading| heading.trim().is_empty())
    {
        return Err(Error::InvalidInput {
            field: "heading",
            reason: "must not be empty",
        });
    }
    validate_optional_input(request.heading.as_deref(), "heading", MAX_PATTERN_BYTES)?;
    if request.heading_occurrence == Some(0) {
        return Err(Error::InvalidInput {
            field: "heading occurrence",
            reason: "must be one-based",
        });
    }
    if request.heading_occurrence.is_some() && request.heading.is_none() {
        return Err(Error::InvalidInput {
            field: "heading occurrence",
            reason: "requires a heading target",
        });
    }
    validate_optional_input(request.expected_hash.as_deref(), "expected hash", 128)?;
    validate_optional_input(
        request.continuation_cursor.as_deref(),
        "continuation cursor",
        256,
    )?;
    if let Some(cursor) = request.continuation_cursor.as_deref() {
        decode_read_cursor(cursor)?;
    }
    validate_relative(&request.path)?;
    let has_line_target = request.start_line.is_some() || request.end_line.is_some();
    if request.symbol.is_some() && has_line_target {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a symbol or line range, not both",
        });
    }
    if request.heading.is_some() && (request.symbol.is_some() || has_line_target) {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a heading, symbol, or line range",
        });
    }
    if request.continuation_cursor.is_some()
        && (request.symbol.is_some() || request.heading.is_some() || has_line_target)
    {
        return Err(Error::InvalidInput {
            field: "read target",
            reason: "must use either a continuation cursor or a new target, not both",
        });
    }
    if request.symbol.is_none()
        && request.heading.is_none()
        && request.continuation_cursor.is_none()
    {
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
        mut request: OutlineRequest,
        cancellation: &CancellationToken,
    ) -> Result<OutlineResponse> {
        check_cancelled(cancellation)?;
        validate_outline_input(&request)?;
        request.paths = request
            .paths
            .iter()
            .map(|path| normalize_relative(path))
            .collect::<Result<Vec<_>>>()?;
        let limit = self.result_limit(request.max_results)?;
        let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let (mut response, baseline_source_tokens) = self.consistent(|session, generation| {
            let offset = parse_outline_cursor(request.cursor.as_deref(), generation, &request)?;
            let mut total_symbols = 0usize;
            let mut total_imports = 0usize;
            let mut symbol_counts_by_kind = BTreeMap::new();
            let mut parse_complete = true;
            let mut files = Vec::with_capacity(request.paths.len());
            let mut file_totals = Vec::with_capacity(request.paths.len());
            for path in &request.paths {
                check_cancelled(cancellation)?;
                let file = session
                    .find_file(path)?
                    .ok_or_else(|| Error::NotIndexed(path.clone()))?;
                let kind_counts = session.symbol_counts_for_file_filtered(
                    file.id,
                    request.symbol_name.as_deref(),
                    request.symbol_kind.as_deref(),
                )?;
                let file_symbol_total = kind_counts.iter().map(|(_, count)| *count).sum::<usize>();
                for (kind, count) in kind_counts {
                    *symbol_counts_by_kind.entry(kind).or_insert(0usize) += count;
                }
                let file_import_total = session.count_imports_for_file(file.id)?;
                total_symbols = total_symbols.saturating_add(file_symbol_total);
                total_imports = total_imports.saturating_add(file_import_total);
                parse_complete &= file.structurally_complete;
                file_totals.push((file.id, file_symbol_total, file_import_total));
                files.push(OutlineFile {
                    path: file.path,
                    language: file.language,
                    parse_complete: file.structurally_complete,
                    structurally_complete: file.structurally_complete,
                    symbols: Vec::new(),
                    imports: Vec::new(),
                });
            }

            let total_entries = total_symbols.saturating_add(total_imports);
            if offset > total_entries {
                return Err(Error::StaleCursor);
            }

            let mut remaining = limit;
            let mut emitted_tokens = 0usize;
            let mut returned_symbols = 0usize;
            let mut returned_imports = 0usize;
            let mut consumed = offset;
            let mut prefix = 0usize;
            let mut truncated_by_max_tokens = false;
            'files: for (file_index, (file_id, file_symbol_total, file_import_total)) in
                file_totals.iter().copied().enumerate()
            {
                check_cancelled(cancellation)?;
                let file_total = file_symbol_total.saturating_add(file_import_total);
                let file_end = prefix.saturating_add(file_total);
                if offset >= file_end {
                    prefix = file_end;
                    continue;
                }
                let local_offset = offset.saturating_sub(prefix);
                let mut symbol_offset = local_offset.min(file_symbol_total);
                while symbol_offset < file_symbol_total {
                    if remaining == 0 {
                        break 'files;
                    }
                    let batch_limit = file_symbol_total.saturating_sub(symbol_offset).min(100);
                    let symbols = session.get_symbols_for_file_filtered_page(
                        file_id,
                        request.symbol_name.as_deref(),
                        request.symbol_kind.as_deref(),
                        batch_limit,
                        symbol_offset,
                    )?;
                    if symbols.is_empty() {
                        return Err(Error::StaleCursor);
                    }
                    for symbol in symbols {
                        if remaining == 0 {
                            break 'files;
                        }
                        consumed = consumed.saturating_add(1);
                        symbol_offset = symbol_offset.saturating_add(1);
                        let symbol = storage_symbol(symbol);
                        let cost = symbol
                            .signature
                            .as_deref()
                            .map_or(1, |value| self.config.tokenizer.count(value));
                        if emitted_tokens.saturating_add(cost) > token_limit {
                            truncated_by_max_tokens = true;
                            continue;
                        }
                        emitted_tokens = emitted_tokens.saturating_add(cost);
                        remaining -= 1;
                        returned_symbols = returned_symbols.saturating_add(1);
                        files[file_index].symbols.push(symbol);
                    }
                }

                let mut import_offset = local_offset.saturating_sub(file_symbol_total);
                while import_offset < file_import_total {
                    if remaining == 0 {
                        break 'files;
                    }
                    let batch_limit = file_import_total.saturating_sub(import_offset).min(100);
                    let imports =
                        session.get_imports_for_file_page(file_id, batch_limit, import_offset)?;
                    if imports.is_empty() {
                        return Err(Error::StaleCursor);
                    }
                    for import in imports {
                        if remaining == 0 {
                            break 'files;
                        }
                        consumed = consumed.saturating_add(1);
                        import_offset = import_offset.saturating_add(1);
                        let import = Import {
                            raw_target: import.raw_target,
                            resolved_path: import.resolved_path,
                            line: import.line,
                        };
                        let cost = self.config.tokenizer.count(&import.raw_target)
                            + import
                                .resolved_path
                                .as_deref()
                                .map_or(0, |value| self.config.tokenizer.count(value));
                        if emitted_tokens.saturating_add(cost) > token_limit {
                            truncated_by_max_tokens = true;
                            continue;
                        }
                        emitted_tokens = emitted_tokens.saturating_add(cost);
                        remaining -= 1;
                        returned_imports = returned_imports.saturating_add(1);
                        files[file_index].imports.push(import);
                    }
                }
                prefix = file_end;
            }

            let truncated_by_max_results = remaining == 0 && consumed < total_entries;
            let next_cursor = truncated_by_max_results
                .then(|| make_outline_cursor(generation, consumed, &request));
            let result_complete = offset == 0
                && returned_symbols == total_symbols
                && returned_imports == total_imports;
            let paths = files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            let baseline_source_tokens =
                session.whole_file_source_tokens(&paths, self.config.tokenizer.name())?;
            Ok((
                OutlineResponse {
                    files,
                    parse_complete,
                    result_complete,
                    total_symbols,
                    returned_symbols,
                    total_imports,
                    returned_imports,
                    truncated_by_max_results,
                    truncated_by_max_tokens,
                    symbol_counts_by_kind,
                    meta: self.meta(generation, emitted_tokens, next_cursor),
                },
                baseline_source_tokens,
            ))
        })?;
        let receipt_candidates = response
            .files
            .iter()
            .flat_map(|file| {
                let symbol_evidence = file.symbols.iter().map(|symbol| {
                    let content = symbol.signature.as_deref().unwrap_or(&symbol.name);
                    ReceiptEvidence::new(
                        file.path.clone(),
                        symbol.start_line,
                        symbol.end_line,
                        hash(content),
                        Some(content),
                    )
                });
                let import_evidence = file.imports.iter().map(|import| {
                    ReceiptEvidence::new(
                        file.path.clone(),
                        import.line,
                        import.line,
                        hash(&import.raw_target),
                        Some(&import.raw_target),
                    )
                });
                symbol_evidence.chain(import_evidence)
            })
            .collect::<Vec<_>>();
        let receipt = self.evaluate_receipt(
            request.receipt_id.as_deref(),
            response.meta.repository_generation,
            &receipt_candidates,
        )?;
        let mut decision_index = 0usize;
        for file in &mut response.files {
            file.symbols.retain(|_| {
                let keep = matches!(
                    receipt.decisions[decision_index],
                    ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                );
                decision_index += 1;
                keep
            });
            file.imports.retain(|_| {
                let keep = matches!(
                    receipt.decisions[decision_index],
                    ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                );
                decision_index += 1;
                keep
            });
        }
        response.returned_symbols = response.files.iter().map(|file| file.symbols.len()).sum();
        response.returned_imports = response.files.iter().map(|file| file.imports.len()).sum();
        response.result_complete = response.result_complete
            && response.returned_symbols == response.total_symbols
            && response.returned_imports == response.total_imports;
        let symbol_tokens = response
            .files
            .iter()
            .flat_map(|file| &file.symbols)
            .map(|symbol| {
                symbol
                    .signature
                    .as_deref()
                    .map_or(1, |signature| self.config.tokenizer.count(signature))
            })
            .sum::<usize>();
        let import_tokens = response
            .files
            .iter()
            .flat_map(|file| &file.imports)
            .map(|import| {
                self.config.tokenizer.count(&import.raw_target)
                    + import
                        .resolved_path
                        .as_deref()
                        .map_or(0, |path| self.config.tokenizer.count(path))
            })
            .sum::<usize>();
        response.meta.source_tokens = symbol_tokens.saturating_add(import_tokens);
        response.meta.emitted_tokens = response.meta.source_tokens;
        receipt.apply_meta(&mut response.meta);
        if let Some(baseline_source_tokens) = baseline_source_tokens {
            self.record_token_savings(
                TokenSavingsOperation::Outline,
                baseline_source_tokens,
                response.meta.emitted_tokens,
            );
        }
        self.finalize_response(&mut response)?;
        Ok(response)
    }

    fn read_sync(
        &self,
        mut request: ReadRequest,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        validate_read_input(&request)?;
        request.path = normalize_relative(&request.path)?;
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens)?;
        let (mut response, baseline_source_tokens) = self.consistent(|session, generation| {
            check_cancelled(cancellation)?;
            self.read_at_generation(session, &request, generation, max_tokens)
        })?;
        let receipt_candidates = response
            .content
            .as_deref()
            .map(|content| {
                vec![ReceiptEvidence::new(
                    response.path.clone(),
                    response.returned_start_line,
                    response.returned_end_line,
                    response.content_hash.clone(),
                    Some(content),
                )]
            })
            .unwrap_or_default();
        let receipt = self.evaluate_receipt(
            request.receipt_id.as_deref(),
            response.meta.repository_generation,
            &receipt_candidates,
        )?;
        if receipt.decisions.first().is_some_and(|decision| {
            matches!(
                decision,
                ReceiptDecision::SuppressExact | ReceiptDecision::SuppressOverlap
            )
        }) {
            response.content = None;
            response.status = ReadStatus::NotModified;
            response.not_modified = true;
            response.meta.source_tokens = 0;
            response.meta.emitted_tokens = 0;
        }
        receipt.apply_meta(&mut response.meta);
        self.record_token_savings(
            TokenSavingsOperation::Read,
            baseline_source_tokens,
            response.meta.emitted_tokens,
        );
        self.finalize_response(&mut response)?;
        Ok(response)
    }

    fn read_at_generation(
        &self,
        session: &ReadSession,
        request: &ReadRequest,
        generation: u64,
        max_tokens: usize,
    ) -> Result<(ReadResponse, usize)> {
        let indexed = session
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let target = resolve_read_target(session, indexed.id, request, generation)?;

        // Stream the file through a BufReader for the full-file hash so the
        // entire file does not need to be held in memory simultaneously. The
        // content range is extracted by a bounded line-oriented reader.
        let file = open_live_file(self, &request.path)?;
        let snapshot = stream_snapshot(&file)?;
        if target
            .expected_full_hash
            .as_deref()
            .is_some_and(|expected| expected != snapshot.content_hash)
        {
            return Err(Error::StaleCursor);
        }
        let target_end_line = target
            .target_end_line
            .unwrap_or(snapshot.end_line)
            .min(snapshot.end_line);
        if target.target_start_line > target_end_line || target.page_start_line > target_end_line {
            return Err(invalid_line_range());
        }
        let range = read_live_range(
            &file,
            target.target_start_line,
            target_end_line,
            target.page_start_byte,
            max_tokens,
            self.config.tokenizer,
        )?;
        if range.page_start_line != target.page_start_line {
            return Err(Error::StaleCursor);
        }
        let baseline_source_tokens = self.config.tokenizer.count(&range.content);
        let (content, emitted_tokens) = self.config.tokenizer.truncate(&range.content, max_tokens);
        let returned_start_line = range.page_start_line;
        let returned_end_line = returned_end_line(returned_start_line, content);
        let next_byte = target.page_start_byte.saturating_add(content.len());
        let truncated = next_byte < range.target_bytes;
        let next_start_line = truncated.then(|| {
            if content.ends_with('\n') {
                returned_end_line.saturating_add(1)
            } else {
                returned_end_line
            }
        });
        if truncated {
            let after_read = stream_snapshot(&file)?;
            if after_read.content_hash != snapshot.content_hash
                || after_read.end_line != snapshot.end_line
            {
                return Err(Error::RetryableConflict(
                    crate::error::RetryableOperation::Retrieval,
                ));
            }
        }
        let continuation_cursor = next_start_line.map(|next_start_line| {
            ReadCursor {
                generation,
                target_start_line: target.target_start_line,
                target_end_line,
                next_start_line,
                next_byte,
                full_hash: snapshot.content_hash.clone(),
                path_hash: read_path_hash(&request.path),
            }
            .encode()
        });
        let content_hash = hash(content);
        let index_stale = indexed.content_hash != snapshot.content_hash;
        let indexed_hash = Some(indexed.content_hash);
        let not_modified = request.expected_hash.as_deref() == Some(content_hash.as_str());
        let status = if truncated {
            ReadStatus::Truncated
        } else if not_modified {
            ReadStatus::NotModified
        } else {
            ReadStatus::Content
        };

        Ok((
            ReadResponse {
                path: request.path.clone(),
                status,
                target_start_line: target.target_start_line,
                target_end_line,
                returned_start_line,
                returned_end_line,
                start_line: returned_start_line,
                end_line: returned_end_line,
                truncated,
                next_start_line,
                continuation_cursor,
                not_modified,
                content: (!not_modified).then(|| content.to_string()),
                content_hash,
                indexed_hash,
                index_stale,
                meta: self.meta(
                    generation,
                    if not_modified { 0 } else { emitted_tokens },
                    None,
                ),
            },
            baseline_source_tokens,
        ))
    }
}

fn decode_read_cursor(cursor: &str) -> Result<ReadCursor> {
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [
        generation,
        kind,
        target_start,
        target_end,
        next_start,
        next_byte,
        full_hash,
        path_hash,
    ] = fields.as_slice()
    else {
        return Err(Error::StaleCursor);
    };
    if *kind != "read"
        || full_hash.len() != crate::text::CONTENT_FINGERPRINT_HEX_LEN
        || path_hash.len() != 16
        || !full_hash.bytes().all(is_lower_hex)
        || !path_hash.bytes().all(is_lower_hex)
    {
        return Err(Error::StaleCursor);
    }
    let cursor = ReadCursor {
        generation: generation.parse().map_err(|_| Error::StaleCursor)?,
        target_start_line: target_start.parse().map_err(|_| Error::StaleCursor)?,
        target_end_line: target_end.parse().map_err(|_| Error::StaleCursor)?,
        next_start_line: next_start.parse().map_err(|_| Error::StaleCursor)?,
        next_byte: next_byte.parse().map_err(|_| Error::StaleCursor)?,
        full_hash: (*full_hash).into(),
        path_hash: (*path_hash).into(),
    };
    if cursor.target_start_line == 0
        || cursor.target_end_line < cursor.target_start_line
        || cursor.next_start_line < cursor.target_start_line
        || cursor.next_start_line > cursor.target_end_line
        || cursor.next_byte == 0
    {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

fn parse_read_cursor(cursor: &str, generation: u64, path: &str) -> Result<ReadCursor> {
    let cursor = decode_read_cursor(cursor)?;
    if cursor.generation != generation || cursor.path_hash != read_path_hash(path) {
        return Err(Error::StaleCursor);
    }
    Ok(cursor)
}

fn read_path_hash(path: &str) -> String {
    blake3::hash(path.as_bytes()).to_hex()[..16].to_string()
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn returned_end_line(start_line: usize, content: &str) -> usize {
    let newline_count = content.bytes().filter(|byte| *byte == b'\n').count();
    start_line
        .saturating_add(newline_count)
        .saturating_sub(usize::from(content.ends_with('\n') && newline_count > 0))
}

fn open_live_file(services: &Services, path: &str) -> Result<File> {
    services
        .repository_root
        .open(path)
        .map(cap_std::fs::File::into_std)
        .map_err(|open_error| {
            // The capability open is authoritative for access. Canonicalization is
            // only used after refusal to preserve the public escape classification.
            match resolve_existing(&services.config.root, path) {
                Err(Error::PathOutsideRoot(external)) => Error::PathOutsideRoot(external),
                _ => Error::Io(open_error),
            }
        })
}

fn resolve_read_target(
    session: &ReadSession,
    file_id: i64,
    request: &ReadRequest,
    generation: u64,
) -> Result<ResolvedReadTarget> {
    if let Some(cursor) = request.continuation_cursor.as_deref() {
        let cursor = parse_read_cursor(cursor, generation, &request.path)?;
        return Ok(ResolvedReadTarget {
            target_start_line: cursor.target_start_line,
            target_end_line: Some(cursor.target_end_line),
            page_start_line: cursor.next_start_line,
            page_start_byte: cursor.next_byte,
            expected_full_hash: Some(cursor.full_hash),
        });
    }

    let (target_start_line, target_end_line) = if let Some(symbol_name) = &request.symbol {
        let symbol =
            session
                .find_symbol(file_id, symbol_name)?
                .ok_or_else(|| Error::SymbolNotFound {
                    path: request.path.clone(),
                    symbol: symbol_name.clone(),
                })?;
        (symbol.start_line, Some(symbol.end_line))
    } else if let Some(heading_name) = &request.heading {
        let occurrence = request.heading_occurrence.unwrap_or(1);
        let heading = session
            .find_markdown_heading(file_id, heading_name, occurrence)?
            .ok_or_else(|| Error::HeadingNotFound {
                path: request.path.clone(),
                heading: heading_name.clone(),
                occurrence,
            })?;
        (heading.start_line, Some(heading.end_line))
    } else {
        (request.start_line.unwrap_or(1), request.end_line)
    };

    if target_start_line == 0
        || target_end_line.is_some_and(|end_line| end_line < target_start_line)
    {
        return Err(invalid_line_range());
    }
    Ok(ResolvedReadTarget {
        target_start_line,
        target_end_line,
        page_start_line: target_start_line,
        page_start_byte: 0,
        expected_full_hash: None,
    })
}

/// Stream a file without loading it into memory and capture its live identity.
fn stream_snapshot(file: &File) -> Result<LiveFileSnapshot> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65_536];
    let mut bytes_seen = 0usize;
    let mut newline_count = 0usize;
    let mut last_byte_was_newline = false;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_seen = bytes_seen.saturating_add(n);
        newline_count =
            newline_count.saturating_add(buf[..n].iter().filter(|byte| **byte == b'\n').count());
        last_byte_was_newline = buf[n - 1] == b'\n';
    }
    let end_line = if bytes_seen == 0 {
        1
    } else {
        newline_count.saturating_add(usize::from(!last_byte_was_newline))
    };
    Ok(LiveFileSnapshot {
        content_hash: hasher.finalize().to_hex()[..crate::text::CONTENT_FINGERPRINT_HEX_LEN]
            .to_string(),
        end_line,
    })
}

/// Read a resolved range without changing its original line terminators.
fn read_live_range(
    file: &File,
    target_start_line: usize,
    target_end_line: usize,
    page_start_byte: usize,
    max_tokens: usize,
    tokenizer: crate::tokens::Tokenizer,
) -> Result<LiveReadRange> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut selected = Vec::with_capacity(LIVE_READ_TOKEN_CHECK_BYTES);
    let mut current_line = 1usize;
    let mut target_finished = false;
    let mut token_bound_reached = false;
    let mut target_bytes = 0usize;
    let mut page_start_line = target_start_line;
    let mut next_token_check = LIVE_READ_TOKEN_CHECK_BYTES;
    let mut utf8_pending = Vec::new();

    while !target_finished {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }

        let mut consumed = 0usize;
        let mut validation_chunk = Vec::new();
        for &byte in buffer {
            let in_target = current_line >= target_start_line && current_line <= target_end_line;
            if in_target {
                validation_chunk.push(byte);
            }
            if in_target {
                if target_bytes < page_start_byte {
                    if byte == b'\n' {
                        page_start_line = current_line.saturating_add(1);
                    }
                } else if !token_bound_reached {
                    selected.push(byte);
                }
                target_bytes = target_bytes.saturating_add(1);
            }
            consumed += 1;
            if byte == b'\n' {
                if target_end_line == current_line {
                    target_finished = true;
                    break;
                }
                current_line = current_line.saturating_add(1);
            }
        }
        reader.consume(consumed);
        validate_utf8_chunk(&mut utf8_pending, &validation_chunk, target_finished)?;

        if !token_bound_reached
            && (target_finished
                || selected.len() >= next_token_check
                || selected.len() >= MAX_LIVE_READ_BYTES)
        {
            match std::str::from_utf8(&selected) {
                Ok(content) if tokenizer.count(content) > max_tokens => {
                    token_bound_reached = true;
                }
                Ok(_) => {
                    if selected.len() >= MAX_LIVE_READ_BYTES {
                        return Err(Error::LimitExceeded);
                    }
                    next_token_check = selected.len().saturating_add(LIVE_READ_TOKEN_CHECK_BYTES);
                }
                Err(error) if error.error_len().is_none() => {
                    if target_finished || selected.len() >= MAX_LIVE_READ_BYTES {
                        return Err(Error::InvalidInput {
                            field: "path",
                            reason: "must identify UTF-8 text",
                        });
                    }
                }
                Err(_) => {
                    return Err(Error::InvalidInput {
                        field: "path",
                        reason: "must identify UTF-8 text",
                    });
                }
            }
        }
    }

    if !utf8_pending.is_empty() {
        return Err(Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        });
    }

    if page_start_byte > target_bytes {
        return Err(Error::StaleCursor);
    }
    let content = String::from_utf8(selected).map_err(|_| Error::InvalidInput {
        field: "path",
        reason: "must identify UTF-8 text",
    })?;
    Ok(LiveReadRange {
        content,
        page_start_line,
        target_bytes,
    })
}

fn validate_utf8_chunk(pending: &mut Vec<u8>, bytes: &[u8], final_chunk: bool) -> Result<()> {
    pending.extend_from_slice(bytes);
    match std::str::from_utf8(pending) {
        Ok(_) => {
            pending.clear();
            Ok(())
        }
        Err(error) if error.error_len().is_none() && !final_chunk => {
            let valid_up_to = error.valid_up_to();
            pending.drain(..valid_up_to);
            Ok(())
        }
        Err(_) => Err(Error::InvalidInput {
            field: "path",
            reason: "must identify UTF-8 text",
        }),
    }
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
        let mut unique_indices = HashMap::new();
        let mut unique_requests = Vec::new();
        let mut request_mapping = Vec::new();
        for (index, (request, file_end_line)) in requests.iter().zip(file_end_lines).enumerate() {
            let Some(request) = request.resolve(file_end_line) else {
                continue;
            };
            let unique_index = *unique_indices.entry(request).or_insert_with(|| {
                let unique_index = unique_requests.len();
                unique_requests.push(request);
                unique_index
            });
            request_mapping.push((index, unique_index));
        }
        let ranges = unique_requests
            .iter()
            .map(|request| (request.file_id, request.start_line, request.end_line))
            .collect::<Vec<_>>();
        let chunks = session.get_chunks_overlapping_batch(&ranges)?;
        let hydrated = unique_requests
            .into_iter()
            .zip(chunks)
            .map(|(request, chunks)| assemble_stored_excerpt(request, &chunks))
            .collect::<Vec<_>>();
        let mut excerpts = vec![None; requests.len()];
        for (index, unique_index) in request_mapping {
            excerpts[index] = hydrated[unique_index].clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_snapshot_hashes_from_the_start_of_a_shared_file_cursor() {
        let mut file = tempfile::tempfile().expect("temporary file");
        std::io::Write::write_all(&mut file, b"first\nsecond\n").expect("write fixture");
        file.seek(SeekFrom::Start(6)).expect("move shared cursor");

        let snapshot = stream_snapshot(&file).expect("snapshot");

        assert_eq!(snapshot.content_hash, hash("first\nsecond\n"));
        assert_eq!(snapshot.end_line, 2);
    }
}
