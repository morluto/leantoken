use std::borrow::Cow;

use similar::TextDiff;
use tokio_util::sync::CancellationToken;

use super::change_receipt::{
    classify_historical_symbol_added, classify_historical_symbol_change,
    classify_historical_symbol_removed,
};
use super::validation::{MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input};
use super::{ServiceCallOptions, Services, validate_positive_request_limit};
use crate::model::{
    HistoricalSymbol, HistoryOperation, HistoryRequest, HistoryResponse, SymbolHistoryCommit,
    TokenAccountingOperation,
};
use crate::repository::{
    GitBlob, git_blob_at_revision, git_diff_identity, git_line_history, normalize_relative,
};
use crate::tokens::ResponseBudget;
use crate::{Error, Result, parser};

const DEFAULT_HISTORY_RESULTS: usize = 20;
const MAX_HISTORY_RESULTS: usize = 100;
const DEFAULT_HISTORY_TOKENS: usize = 8_000;

struct ResolvedHistoricalSymbol {
    symbol: HistoricalSymbol,
    signature: Option<String>,
    content: String,
}

fn char_boundaries(value: &str) -> Vec<usize> {
    let mut boundaries = value
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(value.len());
    boundaries
}

fn symbol_diff_buffer(content: &str) -> Cow<'_, str> {
    if content.is_empty() || content.ends_with('\n') {
        Cow::Borrowed(content)
    } else {
        Cow::Owned(format!("{content}\n"))
    }
}

fn validate_history_request(request: &HistoryRequest) -> Result<()> {
    match &request.operation {
        HistoryOperation::ReadSymbol {
            path,
            symbol,
            revision,
        }
        | HistoryOperation::SymbolLog {
            path,
            symbol,
            revision: Some(revision),
        } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_input(symbol, "symbol", MAX_PATTERN_BYTES)?;
            validate_input(revision, "revision", MAX_PATTERN_BYTES)?;
        }
        HistoryOperation::SymbolLog {
            path,
            symbol,
            revision: None,
        } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_input(symbol, "symbol", MAX_PATTERN_BYTES)?;
        }
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_input(symbol, "symbol", MAX_PATTERN_BYTES)?;
            validate_input(base_revision, "base revision", MAX_PATTERN_BYTES)?;
            validate_input(head_revision, "head revision", MAX_PATTERN_BYTES)?;
        }
    }
    if let Some(max_results) = request.max_results {
        validate_positive_request_limit("max_results", max_results, MAX_HISTORY_RESULTS)?;
    }
    Ok(())
}

fn normalize_operation(operation: HistoryOperation) -> Result<HistoryOperation> {
    Ok(match operation {
        HistoryOperation::ReadSymbol {
            path,
            symbol,
            revision,
        } => HistoryOperation::ReadSymbol {
            path: normalize_relative(&path)?,
            symbol,
            revision,
        },
        HistoryOperation::DiffSymbol {
            path,
            symbol,
            base_revision,
            head_revision,
        } => HistoryOperation::DiffSymbol {
            path: normalize_relative(&path)?,
            symbol,
            base_revision,
            head_revision,
        },
        HistoryOperation::SymbolLog {
            path,
            symbol,
            revision,
        } => HistoryOperation::SymbolLog {
            path: normalize_relative(&path)?,
            symbol,
            revision,
        },
    })
}

impl Services {
    /// Retrieve symbol-aware evidence from immutable Git revisions.
    pub async fn history(&self, request: HistoryRequest) -> Result<HistoryResponse> {
        self.history_with_options(request, ServiceCallOptions::new())
            .await
    }

    /// Retrieve symbol-aware Git evidence under serialized-response controls.
    pub async fn history_with_options(
        &self,
        request: HistoryRequest,
        options: ServiceCallOptions,
    ) -> Result<HistoryResponse> {
        self.history_cancellable_with_options(request, options, CancellationToken::new())
            .await
    }

    /// Retrieve symbol-aware Git evidence with cooperative cancellation.
    pub async fn history_cancellable(
        &self,
        request: HistoryRequest,
        cancellation: CancellationToken,
    ) -> Result<HistoryResponse> {
        self.history_cancellable_with_options(request, ServiceCallOptions::new(), cancellation)
            .await
    }

    /// Retrieve symbol-aware Git evidence under response controls and cancellation.
    pub async fn history_cancellable_with_options(
        &self,
        request: HistoryRequest,
        options: ServiceCallOptions,
        cancellation: CancellationToken,
    ) -> Result<HistoryResponse> {
        let operation = TokenAccountingOperation::History;
        self.observe_service_result(operation, self.validate_call_options(options))?;
        self.observe_service_result(operation, validate_history_request(&request))?;
        let this = self.clone();
        let result = self
            .blocking_executor
            .run(cancellation, move |cancellation| {
                this.history_sync(request, options, cancellation)
            })
            .await;
        self.observe_service_result(operation, result)
    }

    fn history_sync(
        &self,
        request: HistoryRequest,
        options: ServiceCallOptions,
        cancellation: &CancellationToken,
    ) -> Result<HistoryResponse> {
        check_cancelled(cancellation)?;
        validate_history_request(&request)?;
        let max_results = request.max_results.unwrap_or(DEFAULT_HISTORY_RESULTS);
        let max_tokens = self.token_limit(request.max_tokens, DEFAULT_HISTORY_TOKENS)?;
        let operation = normalize_operation(request.operation)?;
        let generation = self.consistent(|_, generation| Ok(generation))?;
        let mut response = match operation {
            HistoryOperation::ReadSymbol {
                path,
                symbol,
                revision,
            } => {
                let mut resolved = self.historical_symbol(&path, &symbol, &revision)?;
                let (content, emitted_tokens) = self
                    .config
                    .tokenizer
                    .truncate(&resolved.content, max_tokens);
                let truncated = content.len() < resolved.content.len();
                resolved.symbol.returned_end_line =
                    returned_end_line(resolved.symbol.target_start_line, content);
                resolved.symbol.truncated = truncated;
                resolved.symbol.content = Some(content.to_owned());
                HistoryResponse {
                    kind: "read_symbol".into(),
                    symbol: Some(resolved.symbol),
                    before: None,
                    after: None,
                    diff: None,
                    diff_truncated: false,
                    semantic_change: None,
                    commits: Vec::new(),
                    result_complete: !truncated,
                    meta: self.meta(generation, emitted_tokens, None),
                }
            }
            HistoryOperation::DiffSymbol {
                path,
                symbol,
                base_revision,
                head_revision,
            } => {
                let revisions =
                    git_diff_identity(&self.config.root, &base_revision, Some(&head_revision))?;
                check_cancelled(cancellation)?;
                let before =
                    self.historical_symbol_optional(&path, &symbol, &revisions.base_revision)?;
                check_cancelled(cancellation)?;
                let after =
                    self.historical_symbol_optional(&path, &symbol, &revisions.head_revision)?;
                if before.is_none() && after.is_none() {
                    return Err(Error::SymbolNotFound {
                        path: format!(
                            "{path}@{}..{}",
                            revisions.base_revision, revisions.head_revision
                        ),
                        symbol,
                    });
                }
                let before_content = before
                    .as_ref()
                    .map_or("", |resolved| resolved.content.as_str());
                let after_content = after
                    .as_ref()
                    .map_or("", |resolved| resolved.content.as_str());
                let before_diff = symbol_diff_buffer(before_content);
                let after_diff = symbol_diff_buffer(after_content);
                let full_diff = TextDiff::from_lines(&before_diff, &after_diff)
                    .unified_diff()
                    .context_radius(3)
                    .header(
                        &format!("{}:{path}", revisions.base_revision),
                        &format!("{}:{path}", revisions.head_revision),
                    )
                    .to_string();
                let total_diff_tokens = self.config.tokenizer.count(&full_diff);
                let (diff, emitted_tokens) = self.config.tokenizer.truncate(&full_diff, max_tokens);
                let diff_truncated = emitted_tokens < total_diff_tokens;
                let semantic_change = match (&before, &after) {
                    (Some(before), Some(after)) => classify_historical_symbol_change(
                        &path,
                        &before.symbol,
                        before.signature.as_deref(),
                        &before.content,
                        &after.symbol,
                        after.signature.as_deref(),
                        &after.content,
                    ),
                    (None, Some(after)) => Some(classify_historical_symbol_added(
                        &path,
                        &after.symbol,
                        after.signature.as_deref(),
                    )),
                    (Some(before), None) => Some(classify_historical_symbol_removed(
                        &path,
                        &before.symbol,
                        before.signature.as_deref(),
                    )),
                    (None, None) => unreachable!("both absent endpoints returned above"),
                };
                let before = before.map(|mut resolved| {
                    resolved.symbol.content = None;
                    resolved.symbol.returned_end_line = 0;
                    resolved.symbol
                });
                let after = after.map(|mut resolved| {
                    resolved.symbol.content = None;
                    resolved.symbol.returned_end_line = 0;
                    resolved.symbol
                });
                HistoryResponse {
                    kind: "diff_symbol".into(),
                    symbol: None,
                    before,
                    after,
                    diff: Some(diff.to_owned()),
                    diff_truncated,
                    semantic_change,
                    commits: Vec::new(),
                    result_complete: !diff_truncated,
                    meta: self.meta(generation, emitted_tokens, None),
                }
            }
            HistoryOperation::SymbolLog {
                path,
                symbol,
                revision,
            } => {
                let revision = revision.as_deref().unwrap_or("HEAD");
                let resolved = self.historical_symbol(&path, &symbol, revision)?;
                check_cancelled(cancellation)?;
                let mut commits = git_line_history(
                    &self.config.root,
                    revision,
                    &path,
                    resolved.symbol.target_start_line,
                    resolved.symbol.target_end_line,
                    max_results.saturating_add(1),
                )?;
                let result_complete = commits.len() <= max_results;
                commits.truncate(max_results);
                HistoryResponse {
                    kind: "symbol_log".into(),
                    symbol: Some(HistoricalSymbol {
                        content: None,
                        returned_end_line: 0,
                        ..resolved.symbol
                    }),
                    before: None,
                    after: None,
                    diff: None,
                    diff_truncated: false,
                    semantic_change: None,
                    commits: commits
                        .into_iter()
                        .map(|commit| SymbolHistoryCommit {
                            commit: commit.commit,
                            authored_at: commit.authored_at,
                            subject: commit.subject,
                        })
                        .collect(),
                    result_complete,
                    meta: self.meta(generation, 0, None),
                }
            }
        };
        self.fit_history_response(&mut response, options)?;
        self.finalize_bounded_response(&mut response, options)?;
        self.record_token_savings(TokenAccountingOperation::History, None, &response.meta);
        Ok(response)
    }

    fn fit_history_response(
        &self,
        response: &mut HistoryResponse,
        options: ServiceCallOptions,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }

        if let Some(content) = response
            .symbol
            .as_ref()
            .and_then(|symbol| symbol.content.as_ref())
            .cloned()
        {
            let boundaries = char_boundaries(&content);
            if let Some(candidate) =
                self.fit_history_text_prefix(response, &content, &boundaries, options, false)?
            {
                *response = candidate;
                return Ok(());
            }
        } else if let Some(diff) = response.diff.clone() {
            let boundaries = char_boundaries(&diff);
            if let Some(candidate) =
                self.fit_history_text_prefix(response, &diff, &boundaries, options, true)?
            {
                *response = candidate;
                return Ok(());
            }
        } else if !response.commits.is_empty() {
            let original = response.clone();
            let max_response_tokens = options
                .max_response_tokens()
                .expect("fitting only runs with a response limit");
            let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
            let keep = budget.largest_fitting_prefix(original.commits.len(), |keep| {
                let mut candidate = original.clone();
                candidate.commits.truncate(keep);
                candidate.result_complete = false;
                self.finalized_response_tokens(&candidate)
            })?;
            if let Some(keep) = keep.filter(|keep| *keep > 0) {
                response.commits.truncate(keep);
                response.result_complete = false;
                return Ok(());
            }
        }

        let requested = self.finalized_response_tokens(response)?;
        Err(Error::RequestLimitExceeded {
            field: "max_response_tokens",
            requested,
            limit: options
                .max_response_tokens()
                .expect("fitting only runs with a response limit"),
        })
    }

    fn fit_history_text_prefix(
        &self,
        response: &HistoryResponse,
        text: &str,
        boundaries: &[usize],
        options: ServiceCallOptions,
        is_diff: bool,
    ) -> Result<Option<HistoryResponse>> {
        let max_response_tokens = options
            .max_response_tokens()
            .expect("fitting only runs with a response limit");
        let budget = ResponseBudget::new(&self.config.tokenizer, max_response_tokens);
        let keep = budget.largest_fitting_prefix(boundaries.len().saturating_sub(1), |keep| {
            let prefix = &text[..boundaries[keep]];
            let mut candidate = response.clone();
            let source_tokens = self.config.tokenizer.count(prefix);
            candidate.meta.source_tokens = source_tokens;
            candidate.meta.emitted_tokens = source_tokens;
            candidate.result_complete = false;
            if is_diff {
                candidate.diff = Some(prefix.to_owned());
                candidate.diff_truncated = true;
            } else if let Some(symbol) = candidate.symbol.as_mut() {
                symbol.content = Some(prefix.to_owned());
                symbol.returned_end_line = returned_end_line(symbol.target_start_line, prefix);
                symbol.truncated = true;
            }
            self.finalized_response_tokens(&candidate)
        })?;
        let Some(keep) = keep.filter(|keep| *keep > 0) else {
            return Ok(None);
        };
        let prefix = &text[..boundaries[keep]];
        let mut candidate = response.clone();
        let source_tokens = self.config.tokenizer.count(prefix);
        candidate.meta.source_tokens = source_tokens;
        candidate.meta.emitted_tokens = source_tokens;
        candidate.result_complete = false;
        if is_diff {
            candidate.diff = Some(prefix.to_owned());
            candidate.diff_truncated = true;
        } else if let Some(symbol) = candidate.symbol.as_mut() {
            symbol.content = Some(prefix.to_owned());
            symbol.returned_end_line = returned_end_line(symbol.target_start_line, prefix);
            symbol.truncated = true;
        }
        Ok(Some(candidate))
    }

    fn historical_symbol(
        &self,
        path: &str,
        symbol_name: &str,
        revision: &str,
    ) -> Result<ResolvedHistoricalSymbol> {
        let blob = git_blob_at_revision(
            &self.config.root,
            revision,
            path,
            usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX),
        )?;
        let resolved_revision = blob.revision.clone();
        self.resolve_historical_symbol(path, symbol_name, blob)?
            .ok_or_else(|| Error::SymbolNotFound {
                path: format!("{path}@{resolved_revision}"),
                symbol: symbol_name.to_owned(),
            })
    }

    fn historical_symbol_optional(
        &self,
        path: &str,
        symbol_name: &str,
        revision: &str,
    ) -> Result<Option<ResolvedHistoricalSymbol>> {
        let blob = match git_blob_at_revision(
            &self.config.root,
            revision,
            path,
            usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX),
        ) {
            Ok(blob) => blob,
            Err(Error::InvalidInput {
                field: "path",
                reason: "file does not exist at revision",
            }) => return Ok(None),
            Err(error) => return Err(error),
        };
        self.resolve_historical_symbol(path, symbol_name, blob)
    }

    fn resolve_historical_symbol(
        &self,
        path: &str,
        symbol_name: &str,
        blob: GitBlob,
    ) -> Result<Option<ResolvedHistoricalSymbol>> {
        let parsed = parser::parse(path, &blob.content)?;
        let mut symbols = parsed.symbols;
        let symbol_index = symbols
            .iter()
            .position(|symbol| symbol.name == symbol_name)
            .or_else(|| {
                symbols.iter().position(|symbol| {
                    symbol.parent.as_deref().is_some_and(|parent| {
                        symbol_name
                            .strip_prefix(parent)
                            .and_then(|suffix| suffix.strip_prefix('.'))
                            == Some(symbol.name.as_str())
                    })
                })
            });
        let Some(symbol_index) = symbol_index else {
            return Ok(None);
        };
        let symbol = symbols.remove(symbol_index);
        let content = blob
            .content
            .get(symbol.start_byte..symbol.end_byte)
            .ok_or_else(|| Error::InternalFailure("invalid historical symbol range".into()))?
            .to_owned();
        let content_hash = crate::text::hash(&content);
        Ok(Some(ResolvedHistoricalSymbol {
            symbol: HistoricalSymbol {
                revision: blob.revision,
                path: path.to_owned(),
                name: symbol.name,
                kind: symbol.kind,
                parent: symbol.parent,
                target_start_line: symbol.start_line,
                target_end_line: symbol.end_line,
                returned_end_line: symbol.end_line,
                truncated: false,
                content: None,
                content_hash,
            },
            signature: symbol.signature,
            content,
        }))
    }
}

fn returned_end_line(start_line: usize, content: &str) -> usize {
    start_line.saturating_add(content.lines().count().saturating_sub(1))
}
