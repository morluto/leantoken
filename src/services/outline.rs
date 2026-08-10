//! Bounded structural outlines.

use std::collections::BTreeMap;

use tokio_util::sync::CancellationToken;

use super::execution_options::RetrievalExecution;
use super::receipts::{ReceiptDecision, ReceiptEvidence};
use super::validation::{
    MAX_INPUT_ITEMS, MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, is_lower_hex,
    validate_input, validate_optional_input,
};
use super::{ServiceCallOptions, Services};
use crate::model::*;
use crate::repository::{normalize_relative, validate_relative};
use crate::text::hash;
use crate::{Error, Result};

fn outline_request_class(response: &OutlineResponse) -> TokenSavingsRequestClass {
    let empty_latex_outline = response.total_symbols == 0
        && response
            .files
            .iter()
            .any(|file| file.language.as_deref() == Some("latex"));
    if !response.parse_complete || empty_latex_outline {
        TokenSavingsRequestClass::Unsupported
    } else if !response.result_complete {
        TokenSavingsRequestClass::Incomplete
    } else {
        TokenSavingsRequestClass::Useful
    }
}

fn outline_signatures_request_class(
    response: &OutlineSignaturesResponse,
) -> TokenSavingsRequestClass {
    let empty_latex_outline = response.total_symbols == 0
        && response
            .files
            .iter()
            .any(|file| file.language.as_deref() == Some("latex"));
    if !response.parse_complete || empty_latex_outline {
        TokenSavingsRequestClass::Unsupported
    } else if !response.result_complete {
        TokenSavingsRequestClass::Incomplete
    } else {
        TokenSavingsRequestClass::Useful
    }
}

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

struct OutlineCursor {
    generation: u64,
    offset: usize,
    query_hash: String,
}

struct ParsedOutlineRequest {
    request: OutlineRequest,
    cursor: Option<OutlineCursor>,
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

fn decode_outline_cursor(cursor: &str) -> Result<OutlineCursor> {
    let fields = cursor.split(':').collect::<Vec<_>>();
    let [generation, kind, offset, query_hash] = fields.as_slice() else {
        return Err(Error::StaleCursor);
    };
    if *kind != "outline" || query_hash.len() != 16 || !query_hash.bytes().all(is_lower_hex) {
        return Err(Error::StaleCursor);
    }
    Ok(OutlineCursor {
        generation: generation.parse().map_err(|_| Error::StaleCursor)?,
        offset: offset.parse().map_err(|_| Error::StaleCursor)?,
        query_hash: (*query_hash).into(),
    })
}

fn outline_query_hash(request: &OutlineRequest, projection: Option<&str>) -> String {
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
    if let Some(projection) = projection {
        hasher.update(&[2]);
        update_field(&mut hasher, projection);
    }
    hasher.finalize().to_hex()[..16].to_string()
}

fn outline_cursor_offset(
    cursor: Option<&OutlineCursor>,
    generation: u64,
    request: &OutlineRequest,
    projection: Option<&str>,
) -> Result<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    if cursor.generation != generation
        || cursor.query_hash != outline_query_hash(request, projection)
    {
        return Err(Error::StaleCursor);
    }
    Ok(cursor.offset)
}

fn make_outline_cursor(
    generation: u64,
    offset: usize,
    request: &OutlineRequest,
    projection: Option<&str>,
) -> String {
    format!(
        "{generation}:outline:{offset}:{}",
        outline_query_hash(request, projection)
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
    let cursor = request
        .cursor
        .take()
        .map(|cursor| decode_outline_cursor(&cursor))
        .transpose()?;
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
        let options = options.with_receipt_resource_reserve();
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
            .blocking_executor
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
            .blocking_executor
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
                this.record_token_savings_classified(
                    TokenAccountingOperation::Outline,
                    None,
                    &compact.meta,
                    outline_signatures_request_class(&compact),
                );
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
        let (mut response, baseline_source_tokens) = self.consistent(|session| {
            let generation = session.generation();
            let cursor_projection = output.cursor_projection();
            let offset =
                outline_cursor_offset(cursor.as_ref(), generation, &request, cursor_projection)?;
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
                .then(|| make_outline_cursor(generation, consumed, &request, cursor_projection));
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
                },
                baseline_source_tokens,
            ))
        })?;
        let returned_entries = response
            .returned_symbols
            .saturating_add(response.returned_imports);
        if !self.response_fits_with_receipt_reserve(&response, returned_entries, options)? {
            return Err(self.response_budget_error_with_receipt_reserve(
                &response,
                returned_entries,
                options
                    .max_response_tokens()
                    .expect("fitting only runs with a response limit"),
                options,
            )?);
        }
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
        receipt.apply_meta(&mut response.meta);
        self.finalize_bounded_response(&mut response, options)?;
        if output == OutlineOutput::Full {
            self.record_token_savings_classified(
                TokenAccountingOperation::Outline,
                baseline_source_tokens,
                &response.meta,
                outline_request_class(&response),
            );
        }
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
        let full = make_outline_cursor(7, 3, &request, None);
        let signatures = make_outline_cursor(7, 3, &request, Some("signatures"));
        let full_cursor = decode_outline_cursor(&full).expect("decode full cursor");
        let signatures_cursor =
            decode_outline_cursor(&signatures).expect("decode signature cursor");
        assert_ne!(full, signatures);
        assert_eq!(
            outline_cursor_offset(Some(&full_cursor), 7, &request, None).expect("full cursor"),
            3
        );
        assert_eq!(
            outline_cursor_offset(Some(&signatures_cursor), 7, &request, Some("signatures"))
                .expect("signature cursor"),
            3
        );
        assert!(matches!(
            outline_cursor_offset(Some(&full_cursor), 7, &request, Some("signatures")),
            Err(Error::StaleCursor)
        ));
        assert!(matches!(
            outline_cursor_offset(Some(&signatures_cursor), 7, &request, None),
            Err(Error::StaleCursor)
        ));
    }
}
