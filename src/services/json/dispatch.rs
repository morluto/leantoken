//! Synchronous JSON dispatcher and token-bounded response fitting.

use serde_json::Value;

use super::cursor::{json_stream_id, make_json_cursor};
use super::execution::JsonExecutionOptions;
use super::keys::{KeyProjectionContext, project_key_page};
use super::numeric::numeric_summary;
use super::projection::{ProjectionState, project_json, projection_item_count};
use super::schema::project_schema_page;
use super::selection::{ParsedJsonSelector, select_json};
use super::source::{JsonMeasurementCache, JsonMeasurementKey, json_tokens};
use super::validation::{ParsedJsonOperation, ParsedJsonRequest};
use super::{DEFAULT_ARRAY_SAMPLE_SIZE, DEFAULT_JSON_ITEMS};
use crate::model::{
    JsonFieldDiff, JsonIncompleteReason, JsonProjection, JsonResponse, TokenAccountingOperation,
};
use crate::services::cursor::StreamId;
use crate::services::validation::check_cancelled;
use crate::services::{ServiceCallOptions, Services};
use crate::tokens::ResponseBudget;
use crate::{Error, Result};

struct JsonLimits {
    max_tokens: usize,
    max_items: usize,
    array_sample_size: usize,
}

struct KeyResponseFit {
    source_hash: String,
    stream_id: StreamId,
    offset: usize,
}

struct JsonOperationResult {
    response: JsonResponse,
    baseline_source_tokens: usize,
    projected_tokens: usize,
    key_response_fit: Option<KeyResponseFit>,
}

struct QueryExecution<'a> {
    path: String,
    selector: Option<ParsedJsonSelector>,
    projection: JsonProjection,
    limits: &'a JsonLimits,
    cursor: Option<&'a super::cursor::JsonCursor>,
    stream_id: StreamId,
    execution: JsonExecutionOptions,
    generation: u64,
    measurements: &'a mut JsonMeasurementCache,
}

struct DiffExecution<'a> {
    base_path: String,
    head_path: String,
    selectors: Vec<ParsedJsonSelector>,
    projection: JsonProjection,
    limits: &'a JsonLimits,
    generation: u64,
    cancellation: &'a tokio_util::sync::CancellationToken,
}

type Completeness = (usize, usize, usize, Option<JsonIncompleteReason>);

fn query_response(
    services: &Services,
    generation: u64,
    source: crate::model::JsonSource,
    value: Value,
    completeness: Option<Completeness>,
    next_cursor: Option<String>,
) -> JsonResponse {
    let (result_complete, total_items, returned_items, remaining_items, incomplete_reason) =
        match completeness {
            Some((total, returned, remaining, reason)) => (
                remaining == 0,
                Some(total),
                Some(returned),
                Some(remaining),
                reason,
            ),
            None => (true, None, None, None, None),
        };
    JsonResponse {
        kind: "query".into(),
        value: Some(value),
        numeric_summary: None,
        differences: Vec::new(),
        sources: vec![source],
        result_complete,
        total_items,
        returned_items,
        remaining_items,
        incomplete_reason,
        meta: services.meta(generation, 0, next_cursor),
    }
}

impl Services {
    pub(super) fn json_sync(
        &self,
        request: ParsedJsonRequest,
        options: ServiceCallOptions,
        execution: JsonExecutionOptions,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<JsonResponse> {
        check_cancelled(cancellation)?;
        let limits = JsonLimits {
            max_tokens: self.token_limit(request.max_tokens, self.config.default_read_tokens)?,
            max_items: request.max_items.unwrap_or(DEFAULT_JSON_ITEMS),
            array_sample_size: request
                .array_sample_size
                .unwrap_or(DEFAULT_ARRAY_SAMPLE_SIZE),
        };
        let generation = self.storage.repository_generation()?;
        let mut measurements = JsonMeasurementCache::default();
        let mut result = match request.operation {
            ParsedJsonOperation::Query {
                path,
                selector,
                projection,
                cursor,
                query_hash,
            } => {
                let stream_id = json_stream_id(self, &query_hash);
                self.execute_json_query(QueryExecution {
                    path,
                    selector,
                    projection,
                    limits: &limits,
                    cursor: cursor.as_ref(),
                    stream_id,
                    execution,
                    generation,
                    measurements: &mut measurements,
                })?
            }
            ParsedJsonOperation::NumericSummary { path, selector } => {
                self.execute_json_numeric_summary(path, selector, generation)?
            }
            ParsedJsonOperation::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
            } => self.execute_json_diff(DiffExecution {
                base_path,
                head_path,
                selectors,
                projection,
                limits: &limits,
                generation,
                cancellation,
            })?,
        };
        if result.projected_tokens > limits.max_tokens {
            return Err(Error::RequestLimitExceeded {
                field: "projected JSON tokens",
                requested: result.projected_tokens,
                limit: limits.max_tokens,
            });
        }
        result.response.meta.source_tokens = result.projected_tokens;
        self.fit_json_response(
            &mut result.response,
            result.key_response_fit.as_ref(),
            options,
            &mut measurements,
        )?;
        self.finalize_bounded_response(&mut result.response, options)?;
        self.record_token_savings(
            TokenAccountingOperation::Json,
            Some(result.baseline_source_tokens),
            &result.response.meta,
        );
        Ok(result.response)
    }

    fn execute_json_query(&self, input: QueryExecution<'_>) -> Result<JsonOperationResult> {
        let QueryExecution {
            path,
            selector,
            projection,
            limits,
            cursor,
            stream_id,
            execution,
            generation,
            measurements,
        } = input;
        let loaded = self.load_json(&path)?;
        let baseline_source_tokens = loaded.source_tokens();
        let value = select_json(loaded.value(), selector.as_ref())?.into_required_value()?;
        let source_hash = loaded.source().content_hash.clone();
        let (response, projected_tokens, key_response_fit) = match projection {
            JsonProjection::Keys => {
                let offset = cursor
                    .map(|cursor| cursor.offset_for(&source_hash, stream_id))
                    .transpose()?
                    .unwrap_or(0);
                let page = project_key_page(
                    self,
                    &value,
                    limits.max_items,
                    limits.max_tokens,
                    KeyProjectionContext::new(
                        cursor,
                        &source_hash,
                        stream_id,
                        execution,
                        measurements,
                    ),
                )?;
                let (value, total, returned, remaining, reason, next_cursor, tokens) =
                    page.into_parts();
                (
                    query_response(
                        self,
                        generation,
                        loaded.into_source(),
                        value,
                        Some((total, returned, remaining, reason)),
                        next_cursor,
                    ),
                    tokens,
                    Some(KeyResponseFit {
                        source_hash,
                        stream_id,
                        offset,
                    }),
                )
            }
            JsonProjection::Schema => {
                let page = project_schema_page(self, &value, limits.max_items, limits.max_tokens)?;
                let (value, total, returned, remaining, reason, tokens) = page.into_parts();
                let completeness = (remaining > 0).then_some((total, returned, remaining, reason));
                (
                    query_response(
                        self,
                        generation,
                        loaded.into_source(),
                        value,
                        completeness,
                        None,
                    ),
                    tokens,
                    None,
                )
            }
            _ => {
                let total = projection_item_count(&value, projection, limits.array_sample_size);
                let mut state = ProjectionState::new(limits.max_items, limits.array_sample_size);
                let value = project_json(&value, projection, &mut state)?;
                let tokens = json_tokens(self, &value)?;
                let returned = total.min(limits.max_items);
                let remaining = total.saturating_sub(returned);
                let completeness = (!state.is_complete()).then_some((
                    total,
                    returned,
                    remaining,
                    Some(JsonIncompleteReason::MaxItems),
                ));
                (
                    query_response(
                        self,
                        generation,
                        loaded.into_source(),
                        value,
                        completeness,
                        None,
                    ),
                    tokens,
                    None,
                )
            }
        };
        Ok(JsonOperationResult {
            response,
            baseline_source_tokens,
            projected_tokens,
            key_response_fit,
        })
    }

    fn execute_json_numeric_summary(
        &self,
        path: String,
        selector: Option<ParsedJsonSelector>,
        generation: u64,
    ) -> Result<JsonOperationResult> {
        let loaded = self.load_json(&path)?;
        let baseline_source_tokens = loaded.source_tokens();
        let value = select_json(loaded.value(), selector.as_ref())?.into_required_value()?;
        Ok(JsonOperationResult {
            response: JsonResponse {
                kind: "numeric_summary".into(),
                value: None,
                numeric_summary: Some(numeric_summary(&value)),
                differences: Vec::new(),
                sources: vec![loaded.into_source()],
                result_complete: true,
                total_items: None,
                returned_items: None,
                remaining_items: None,
                incomplete_reason: None,
                meta: self.meta(generation, 0, None),
            },
            baseline_source_tokens,
            projected_tokens: 0,
            key_response_fit: None,
        })
    }

    fn execute_json_diff(&self, input: DiffExecution<'_>) -> Result<JsonOperationResult> {
        let DiffExecution {
            base_path,
            head_path,
            selectors,
            projection,
            limits,
            generation,
            cancellation,
        } = input;
        let before = self.load_json(&base_path)?;
        check_cancelled(cancellation)?;
        let after = self.load_json(&head_path)?;
        let baseline_source_tokens = before.source_tokens().saturating_add(after.source_tokens());
        let mut state = ProjectionState::new(limits.max_items, limits.array_sample_size);
        let mut differences = Vec::with_capacity(selectors.len());
        let mut total_items = 0usize;
        let mut returned_items = 0usize;
        let mut remaining_items = 0usize;
        let mut projected_tokens = 0usize;
        let mut incomplete = false;
        for selector in selectors {
            check_cancelled(cancellation)?;
            let before_selected = select_json(before.value(), Some(&selector))?;
            let after_selected = select_json(after.value(), Some(&selector))?;
            let changed = before_selected.is_present() != after_selected.is_present()
                || before_selected.value() != after_selected.value();
            for selected in [&before_selected, &after_selected] {
                total_items = total_items.saturating_add(
                    selected
                        .value()
                        .map(|value| {
                            projection_item_count(value, projection, limits.array_sample_size)
                        })
                        .unwrap_or_default(),
                );
            }
            let mut project = |value: Option<&Value>| -> Result<Option<Value>> {
                let Some(value) = value else { return Ok(None) };
                if projection == JsonProjection::Schema {
                    let item_budget = limits.max_items.saturating_sub(returned_items);
                    let token_budget = limits.max_tokens.saturating_sub(projected_tokens);
                    if item_budget == 0 || token_budget == 0 {
                        return Ok(None);
                    }
                    let page = project_schema_page(self, value, item_budget, token_budget)?;
                    let (projected, _total, returned, remaining, reason, tokens) =
                        page.into_parts();
                    returned_items = returned_items.saturating_add(returned);
                    remaining_items = remaining_items.saturating_add(remaining);
                    incomplete |= reason.is_some();
                    projected_tokens = projected_tokens.saturating_add(tokens);
                    Ok(Some(projected))
                } else {
                    project_json(value, projection, &mut state).map(Some)
                }
            };
            let before_value = project(before_selected.value())?;
            let after_value = project(after_selected.value())?;
            for value in [&before_value, &after_value].into_iter().flatten() {
                if projection != JsonProjection::Schema {
                    projected_tokens = projected_tokens.saturating_add(json_tokens(self, value)?);
                }
            }
            differences.push(JsonFieldDiff {
                selector: selector.into_wire(),
                before_present: before_selected.is_present(),
                before: before_value,
                after_present: after_selected.is_present(),
                after: after_value,
                changed,
            });
        }
        if projection != JsonProjection::Schema {
            returned_items = total_items.min(limits.max_items);
            remaining_items = total_items.saturating_sub(returned_items);
            incomplete = !state.is_complete();
        }
        Ok(JsonOperationResult {
            response: JsonResponse {
                kind: "diff_fields".into(),
                value: None,
                numeric_summary: None,
                differences,
                sources: vec![before.into_source(), after.into_source()],
                result_complete: !incomplete,
                total_items: incomplete.then_some(total_items),
                returned_items: incomplete.then_some(returned_items),
                remaining_items: incomplete.then_some(remaining_items),
                incomplete_reason: incomplete.then_some(JsonIncompleteReason::MaxItems),
                meta: self.meta(generation, 0, None),
            },
            baseline_source_tokens,
            projected_tokens,
            key_response_fit: None,
        })
    }

    fn fit_json_response(
        &self,
        response: &mut JsonResponse,
        key_page_context: Option<&KeyResponseFit>,
        options: ServiceCallOptions,
        measurements: &mut JsonMeasurementCache,
    ) -> Result<()> {
        if self.response_fits(response, options)? {
            return Ok(());
        }

        if let (Some(context), Some(Value::Array(entries))) =
            (key_page_context, response.value.as_ref())
        {
            let original = response.clone();
            let max_response_tokens = options
                .max_response_tokens()
                .expect("fitting only runs with a response limit");
            let budget = ResponseBudget::new(max_response_tokens);
            let keep = budget.largest_fitting_prefix(entries.len(), |keep| {
                let mut candidate = original.clone();
                let mut value = entries.clone();
                value.truncate(keep);
                candidate.value = Some(Value::Array(value));
                let consumed = context.offset.saturating_add(keep);
                let total = candidate.total_items.unwrap_or(consumed);
                let remaining = total.saturating_sub(consumed);
                candidate.returned_items = Some(keep);
                candidate.remaining_items = Some(remaining);
                candidate.result_complete = remaining == 0;
                candidate.incomplete_reason =
                    (remaining > 0).then_some(JsonIncompleteReason::MaxTokens);
                candidate.meta.next_cursor = (remaining > 0)
                    .then(|| make_json_cursor(context.stream_id, &context.source_hash, consumed))
                    .transpose()?;
                let source_tokens = measurements.measure(
                    self,
                    JsonMeasurementKey::KeysPrefix(keep),
                    candidate
                        .value
                        .as_ref()
                        .expect("keys projection keeps a value"),
                )?;
                candidate.meta.source_tokens = source_tokens;
                self.finalized_response_tokens(&candidate, options)
            })?;
            if let Some(keep) = keep.filter(|keep| *keep > 0) {
                let mut value = entries.clone();
                value.truncate(keep);
                response.value = Some(Value::Array(value));
                let consumed = context.offset.saturating_add(keep);
                let total = response.total_items.unwrap_or(consumed);
                let remaining = total.saturating_sub(consumed);
                response.returned_items = Some(keep);
                response.remaining_items = Some(remaining);
                response.result_complete = remaining == 0;
                response.incomplete_reason =
                    (remaining > 0).then_some(JsonIncompleteReason::MaxTokens);
                response.meta.next_cursor = (remaining > 0)
                    .then(|| make_json_cursor(context.stream_id, &context.source_hash, consumed))
                    .transpose()?;
                let source_tokens = measurements.measure(
                    self,
                    JsonMeasurementKey::KeysPrefix(keep),
                    response
                        .value
                        .as_ref()
                        .expect("keys projection keeps a value"),
                )?;
                response.meta.source_tokens = source_tokens;
                return Ok(());
            }
            if !entries.is_empty() {
                let mut minimum = original;
                let mut value = entries.clone();
                value.truncate(1);
                minimum.value = Some(Value::Array(value));
                let consumed = context.offset.saturating_add(1);
                let total = minimum.total_items.unwrap_or(consumed);
                let remaining = total.saturating_sub(consumed);
                minimum.returned_items = Some(1);
                minimum.remaining_items = Some(remaining);
                minimum.result_complete = remaining == 0;
                minimum.incomplete_reason =
                    (remaining > 0).then_some(JsonIncompleteReason::MaxTokens);
                minimum.meta.next_cursor = (remaining > 0)
                    .then(|| make_json_cursor(context.stream_id, &context.source_hash, consumed))
                    .transpose()?;
                let source_tokens = measurements.measure(
                    self,
                    JsonMeasurementKey::KeysPrefix(1),
                    minimum
                        .value
                        .as_ref()
                        .expect("keys projection keeps a value"),
                )?;
                minimum.meta.source_tokens = source_tokens;
                return Err(self.response_budget_error(
                    &minimum,
                    options
                        .max_response_tokens()
                        .expect("fitting only runs with a response limit"),
                    options,
                )?);
            }
        }

        Err(self.response_budget_error(
            response,
            options
                .max_response_tokens()
                .expect("fitting only runs with a response limit"),
            options,
        )?)
    }
}
