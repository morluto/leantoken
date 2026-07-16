//! Bounded live reads, outlines, and index-backed excerpts.

use std::fs;

use tokio_util::sync::CancellationToken;

use super::Services;
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input,
    validate_optional_input,
};
use crate::model::*;
use crate::repository::{resolve_existing, validate_relative};
use crate::storage::ReadSession;
use crate::text::hash;
use crate::{Error, Result};

const MIN_CONTEXT_RANGE_LINES: usize = 12;
const MAX_CONTEXT_RANGE_LINES: usize = 128;

pub(super) struct StoredExcerpt {
    pub(super) content: String,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
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

impl Services {
    pub(super) fn outline_sync(
        &self,
        request: OutlineRequest,
        cancellation: &CancellationToken,
    ) -> Result<OutlineResponse> {
        check_cancelled(cancellation)?;
        if request.paths.is_empty() {
            return Err(Error::InvalidRequest(
                "outline requires at least one path".into(),
            ));
        }
        if request.paths.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for path in &request.paths {
            validate_input(path, "path", MAX_PATH_BYTES)?;
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
        self.consistent(|session, generation| {
            let limit = self.result_limit(request.max_results);
            let token_limit = self.token_limit(request.max_tokens, self.config.default_read_tokens);
            let mut remaining = limit;
            let mut emitted_tokens = 0usize;
            let mut files = Vec::new();
            for path in &request.paths {
                check_cancelled(cancellation)?;
                validate_relative(path)?;
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

    pub(super) fn read_sync(
        &self,
        request: ReadRequest,
        cancellation: &CancellationToken,
    ) -> Result<ReadResponse> {
        check_cancelled(cancellation)?;
        validate_input(&request.path, "path", MAX_PATH_BYTES)?;
        validate_optional_input(request.symbol.as_deref(), "symbol", MAX_PATTERN_BYTES)?;
        validate_optional_input(request.expected_hash.as_deref(), "expected hash", 128)?;
        validate_relative(&request.path)?;
        if request.symbol.is_some() && (request.start_line.is_some() || request.end_line.is_some())
        {
            return Err(Error::InvalidRequest(
                "read accepts either symbol or line range, not both".into(),
            ));
        }
        self.consistent(|session, generation| {
            check_cancelled(cancellation)?;
            self.read_at_generation(session, &request, generation)
        })
    }

    pub(super) fn read_at_generation(
        &self,
        session: &ReadSession,
        request: &ReadRequest,
        generation: u64,
    ) -> Result<ReadResponse> {
        let indexed = session
            .find_file(&request.path)?
            .ok_or_else(|| Error::NotIndexed(request.path.clone()))?;
        let path = resolve_existing(&self.config.root, &request.path)?;
        let bytes = fs::read(path)?;
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidRequest("requested file is not UTF-8 text".into()))?;
        let line_count = source.lines().count().max(1);

        let (start_line, requested_end) = if let Some(symbol_name) = &request.symbol {
            let symbol = session
                .find_symbol(indexed.id, symbol_name)?
                .ok_or_else(|| Error::NotIndexed(format!("{}::{symbol_name}", request.path)))?;
            (symbol.start_line, symbol.end_line)
        } else {
            (
                request.start_line.unwrap_or(1),
                request.end_line.unwrap_or(line_count),
            )
        };
        if start_line == 0 || requested_end < start_line || start_line > line_count {
            return Err(Error::InvalidRequest(
                "invalid or out-of-range line range".into(),
            ));
        }
        let requested_end = requested_end.min(line_count);
        let content = crate::text::excerpt(source, start_line, requested_end);
        let max_tokens = self.token_limit(request.max_tokens, self.config.default_read_tokens);
        let (content, emitted_tokens) = self.config.tokenizer.truncate(&content, max_tokens);
        let returned_lines = content
            .lines()
            .count()
            .max(usize::from(!content.is_empty()));
        let end_line = if returned_lines == 0 {
            start_line
        } else {
            start_line + returned_lines - 1
        };
        let content_hash = hash(content);
        let full_hash = crate::text::hash_bytes(&bytes);
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

    pub(super) fn stored_excerpt(
        &self,
        session: &ReadSession,
        file_id: i64,
        start_line: usize,
        end_line: usize,
        context: usize,
        max_lines: usize,
    ) -> Result<Option<StoredExcerpt>> {
        let first = start_line.saturating_sub(context).max(1);
        let mut last = end_line.saturating_add(context);
        if max_lines > 0 && last.saturating_sub(first).saturating_add(1) > max_lines {
            last = first + max_lines - 1;
        }
        let selected = session.get_chunks_overlapping(file_id, first, last)?;
        let (Some(first_chunk), Some(last_chunk)) = (selected.first(), selected.last()) else {
            return Ok(None);
        };
        last = last.min(last_chunk.end_line);
        let base_line = first_chunk.start_line;
        let mut combined = String::new();
        for chunk in &selected {
            combined.push_str(&chunk.content);
        }
        let local_start = first.saturating_sub(base_line) + 1;
        let local_end = last.saturating_sub(base_line) + 1;
        Ok(Some(StoredExcerpt {
            content: crate::text::excerpt(&combined, local_start, local_end),
            start_line: first,
            end_line: last,
        }))
    }

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
}
