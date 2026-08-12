//! Bounded structural outlines.

use std::collections::BTreeMap;

use tokio_util::sync::CancellationToken;

use super::cursor::request_digest;
use super::execution_options::RetrievalExecution;
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input,
    validate_optional_input,
};
use super::{ServiceCallOptions, Services};
use crate::model::*;
use crate::repository::{normalize_relative, validate_relative};
use crate::text::hash;
use crate::{Error, Result};

fn storage_symbol(symbol: super::index_read::SymbolRecord) -> Symbol {
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

#[derive(serde::Serialize, serde::Deserialize)]
struct OutlinePosition {
    offset: usize,
}

struct ParsedOutlineRequest {
    request: OutlineRequest,
    cursor: Option<String>,
    limit: usize,
    token_limit: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutlineOutput {
    Full,
    Signatures,
}

impl OutlineOutput {
    const fn includes_imports(self) -> bool {
        matches!(self, Self::Full)
    }

    const fn cursor_projection(self) -> Option<&'static str> {
        match self {
            Self::Full => None,
            Self::Signatures => Some("signatures"),
        }
    }
}

fn outline_request_digest(request: &OutlineRequest, projection: Option<&str>) -> Result<String> {
    request_digest(
        "outline",
        &(
            &request.paths,
            &request.symbol_name,
            &request.symbol_kind,
            projection,
        ),
    )
}

fn outline_cursor_offset(
    services: &Services,
    cursor: Option<&str>,
    generation: u64,
    request: &OutlineRequest,
    projection: Option<&str>,
) -> Result<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let digest = outline_request_digest(request, projection)?;
    let position: OutlinePosition = services.cursor_codec.open(cursor, generation, &digest)?;
    Ok(position.offset)
}

fn make_outline_cursor(
    services: &Services,
    generation: u64,
    offset: usize,
    request: &OutlineRequest,
    projection: Option<&str>,
) -> Result<String> {
    services.cursor_codec.seal(
        generation,
        &outline_request_digest(request, projection)?,
        &OutlinePosition { offset },
    )
}

fn parse_outline_input(
    services: &Services,
    mut request: OutlineRequest,
) -> Result<ParsedOutlineRequest> {
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
    let cursor = request.cursor.take();
    request.paths = request
        .paths
        .iter()
        .map(|path| normalize_relative(path))
        .collect::<Result<Vec<_>>>()?;
    Ok(ParsedOutlineRequest {
        limit: services.result_limit(request.max_results)?,
        token_limit: services
            .token_limit(request.max_tokens, services.config.default_read_tokens)?,
        request,
        cursor,
    })
}

impl Services {
    pub async fn outline(&self, request: OutlineRequest) -> Result<OutlineResponse> {
        self.outline_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Return outlines under explicit serialized-response controls.
    pub async fn outline_with_options(
        &self,
        request: OutlineRequest,
        options: ServiceCallOptions,
    ) -> Result<OutlineResponse> {
        self.outline_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Outline files after applying the requested index consistency boundary.
    pub async fn outline_with_consistency_cancellable(
        &self,
        request: OutlineRequest,
        consistency: IndexConsistency,
        cancellation: CancellationToken,
    ) -> Result<OutlineResponse> {
        self.outline_execute(
            request,
            RetrievalExecution::consistent(consistency, ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    /// Outline files under consistency and serialized-response controls.
    pub async fn outline_with_options_consistency_cancellable(
        &self,
        request: OutlineRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<OutlineResponse> {
        self.outline_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    pub async fn outline_cancellable(
        &self,
        request: OutlineRequest,
        cancellation: CancellationToken,
    ) -> Result<OutlineResponse> {
        self.outline_execute(
            request,
            RetrievalExecution::direct(ServiceCallOptions::new(), cancellation),
        )
        .await
    }

    async fn outline_execute(
        &self,
        request: OutlineRequest,
        execution: RetrievalExecution,
    ) -> Result<OutlineResponse> {
        let operation = TokenAccountingOperation::Outline;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let request = self.observe_service_result(operation, parse_outline_input(self, request))?;
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
            .process_budget
            .run(cancellation, move |cancellation| {
                this.outline_sync(request, options, OutlineOutput::Full, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    /// Outline only symbol signatures, line ranges, and verifiable content hashes.
    pub async fn outline_signatures(
        &self,
        request: OutlineRequest,
    ) -> Result<OutlineSignaturesResponse> {
        self.outline_signatures_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Outline signatures under an exact serialized-response bound.
    pub async fn outline_signatures_with_options(
        &self,
        request: OutlineRequest,
        options: ServiceCallOptions,
    ) -> Result<OutlineSignaturesResponse> {
        self.outline_signatures_execute(
            request,
            RetrievalExecution::direct(options, CancellationToken::new()),
        )
        .await
    }

    /// Outline signatures after applying the requested consistency boundary.
    pub async fn outline_signatures_with_options_consistency_cancellable(
        &self,
        request: OutlineRequest,
        consistency: IndexConsistency,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<OutlineSignaturesResponse> {
        self.outline_signatures_execute(
            request,
            RetrievalExecution::consistent(consistency, options, cancellation),
        )
        .await
    }

    async fn outline_signatures_execute(
        &self,
        request: OutlineRequest,
        execution: RetrievalExecution,
    ) -> Result<OutlineSignaturesResponse> {
        let operation = TokenAccountingOperation::Outline;
        let RetrievalExecution {
            consistency,
            options,
            cancellation,
        } = execution;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        let request = self.observe_service_result(operation, parse_outline_input(self, request))?;
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
            .process_budget
            .run(cancellation, move |cancellation| {
                let response = this.outline_sync(
                    request,
                    ServiceCallOptions::new(),
                    OutlineOutput::Signatures,
                    cancellation,
                )?;
                let mut files = Vec::with_capacity(response.files.len());
                for file in response.files {
                    let signatures = file
                        .symbols
                        .into_iter()
                        .map(|symbol| OutlineSignature {
                            name: symbol.name,
                            kind: symbol.kind,
                            parent: symbol.parent,
                            signature: symbol.signature,
                            start_line: symbol.start_line,
                            end_line: symbol.end_line,
                        })
                        .collect::<Vec<_>>();
                    let serialized = serde_json::to_string(&signatures)
                        .map_err(|error| Error::SerializationFailure(error.to_string()))?;
                    files.push(OutlineSignaturesFile {
                        path: file.path,
                        content_hash: hash(&serialized),
                        language: file.language,
                        parse_complete: file.parse_complete,
                        signatures,
                    });
                }
                let mut compact = OutlineSignaturesResponse {
                    files,
                    path_results: response.path_results,
                    parse_complete: response.parse_complete,
                    result_complete: response.result_complete,
                    total_symbols: response.total_symbols,
                    returned_symbols: response.returned_symbols,
                    truncated_by_max_results: response.truncated_by_max_results,
                    truncated_by_max_tokens: response.truncated_by_max_tokens,
                    symbol_counts_by_kind: response.symbol_counts_by_kind,
                    meta: response.meta,
                };
                this.finalize_bounded_response(&mut compact, options)?;
                Ok(compact)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn outline_sync(
        &self,
        parsed: ParsedOutlineRequest,
        options: ServiceCallOptions,
        output: OutlineOutput,
        cancellation: &CancellationToken,
    ) -> Result<OutlineResponse> {
        check_cancelled(cancellation)?;
        let ParsedOutlineRequest {
            request,
            cursor,
            limit,
            token_limit,
        } = parsed;
        let mut response = self.consistent(|session| {
            let generation = session.generation();
            let cursor_projection = output.cursor_projection();
            let offset = outline_cursor_offset(
                self,
                cursor.as_deref(),
                generation,
                &request,
                cursor_projection,
            )?;
            let mut total_symbols = 0usize;
            let mut total_imports = 0usize;
            let mut symbol_counts_by_kind = BTreeMap::new();
            let mut parse_complete = true;
            let mut all_paths_indexed = true;
            let mut files = Vec::with_capacity(request.paths.len());
            let mut file_totals = Vec::with_capacity(request.paths.len());
            let mut path_results = Vec::with_capacity(request.paths.len());
            for (request_index, path) in request.paths.iter().enumerate() {
                check_cancelled(cancellation)?;
                let Some(file) = session.find_file(path)? else {
                    parse_complete = false;
                    all_paths_indexed = false;
                    path_results.push(OutlinePathResult {
                        request_index,
                        path: path.clone(),
                        status: OutlinePathStatus::NotIndexed,
                    });
                    continue;
                };
                path_results.push(OutlinePathResult {
                    request_index,
                    path: path.clone(),
                    status: OutlinePathStatus::Indexed,
                });
                let kind_counts = session.symbol_counts_for_file_filtered(
                    file.id,
                    request.symbol_name.as_deref(),
                    request.symbol_kind.as_deref(),
                )?;
                let file_symbol_total = kind_counts.iter().map(|(_, count)| *count).sum::<usize>();
                for (kind, count) in kind_counts {
                    *symbol_counts_by_kind.entry(kind).or_insert(0usize) += count;
                }
                let file_import_total = if output.includes_imports() {
                    session.count_imports_for_file(file.id)?
                } else {
                    0
                };
                total_symbols = total_symbols.saturating_add(file_symbol_total);
                total_imports = total_imports.saturating_add(file_import_total);
                parse_complete &= file.structurally_complete;
                file_totals.push((file.id, file_symbol_total, file_import_total));
                files.push(OutlineFile {
                    path: file.path,
                    language: file.language,
                    parse_complete: file.structurally_complete,
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
                .then(|| {
                    make_outline_cursor(self, generation, consumed, &request, cursor_projection)
                })
                .transpose()?;
            let result_complete = offset == 0
                && returned_symbols == total_symbols
                && returned_imports == total_imports;
            Ok(OutlineResponse {
                files,
                path_results,
                parse_complete,
                result_complete: result_complete && all_paths_indexed,
                total_symbols,
                returned_symbols,
                total_imports,
                returned_imports,
                truncated_by_max_results,
                truncated_by_max_tokens,
                symbol_counts_by_kind,
                meta: self.meta(generation, emitted_tokens, next_cursor),
            })
        })?;
        if !self.response_fits(&response, options)? {
            return Err(self.response_budget_error(
                &response,
                options
                    .max_response_tokens()
                    .expect("fitting only runs with a response limit"),
                options,
            )?);
        }
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
        self.finalize_bounded_response(&mut response, options)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_cursors_bind_signature_projection_offsets() {
        let request = OutlineRequest {
            paths: vec!["src/lib.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(10),
            max_tokens: Some(1_000),
            receipt_id: None,
            cursor: None,
        };
        let codec = super::super::cursor::CursorCodec::new("repository".into());
        let full_digest = outline_request_digest(&request, None).expect("full digest");
        let signatures_digest =
            outline_request_digest(&request, Some("signatures")).expect("signature digest");
        let full = codec
            .seal(7, &full_digest, &OutlinePosition { offset: 3 })
            .expect("full cursor");
        let signatures = codec
            .seal(7, &signatures_digest, &OutlinePosition { offset: 3 })
            .expect("signature cursor");
        assert_ne!(full, signatures);
        assert_eq!(
            codec
                .open::<OutlinePosition>(&full, 7, &full_digest)
                .expect("full cursor")
                .offset,
            3
        );
        assert_eq!(
            codec
                .open::<OutlinePosition>(&signatures, 7, &signatures_digest)
                .expect("signature cursor")
                .offset,
            3
        );
        assert!(matches!(
            codec.open::<OutlinePosition>(&full, 7, &signatures_digest),
            Err(Error::StaleCursor)
        ));
        assert!(matches!(
            codec.open::<OutlinePosition>(&signatures, 7, &full_digest),
            Err(Error::StaleCursor)
        ));
    }
}
