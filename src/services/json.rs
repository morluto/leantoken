use std::collections::BTreeMap;
use std::io::Read;

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::read::open_live_file;
use super::validation::{MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input};
use super::{Services, validate_positive_request_limit, validate_request_limit};
use crate::model::{
    JsonFieldDiff, JsonIncompleteReason, JsonNumericSummary, JsonOperation, JsonProjection,
    JsonRequest, JsonResponse, JsonSelector, JsonSource, TokenAccountingOperation,
};
use crate::repository::normalize_relative;
use crate::text::CONTENT_FINGERPRINT_HEX_LEN;
use crate::{Error, Result};

const DEFAULT_JSON_TOKENS: usize = 8_000;
const DEFAULT_JSON_ITEMS: usize = 1_000;
const MAX_JSON_ITEMS: usize = 10_000;
const DEFAULT_ARRAY_SAMPLE_SIZE: usize = 3;
const MAX_ARRAY_SAMPLE_SIZE: usize = 20;
const MAX_JSON_SELECTORS: usize = 100;
const MAX_JSON_CURSOR_BYTES: usize = 256;

struct LoadedJson {
    source: JsonSource,
    value: Value,
    source_tokens: usize,
}

struct SelectedJson {
    present: bool,
    value: Option<Value>,
}

struct ProjectionState {
    remaining: usize,
    array_sample_size: usize,
    complete: bool,
}

struct JsonCursor {
    source_hash: String,
    query_hash: String,
    offset: usize,
}

struct KeyProjectionPage {
    value: Value,
    total_items: usize,
    returned_items: usize,
    remaining_items: usize,
    incomplete_reason: Option<JsonIncompleteReason>,
    next_cursor: Option<String>,
    projected_tokens: usize,
}

fn invalid_json_selector(stage: &'static str, error: jmespath::JmespathError) -> Error {
    Error::InvalidJsonSelector {
        stage,
        offset: error.offset,
        line: error.line.saturating_add(1),
        column: error.column.saturating_add(1),
        reason: error.reason.to_string(),
    }
}

fn validate_selector(selector: &JsonSelector) -> Result<()> {
    match selector {
        JsonSelector::Pointer { pointer } => {
            validate_input(pointer, "JSON Pointer", MAX_PATTERN_BYTES)?;
            if !pointer.is_empty() && !pointer.starts_with('/') {
                return Err(Error::InvalidInput {
                    field: "JSON Pointer",
                    reason: "must be empty or start with a slash",
                });
            }
        }
        JsonSelector::Jmespath { expression } => {
            validate_input(expression, "JMESPath expression", MAX_PATTERN_BYTES)?;
            if expression.trim().is_empty() {
                return Err(Error::InvalidInput {
                    field: "JMESPath expression",
                    reason: "must not be empty",
                });
            }
            jmespath::compile(expression)
                .map_err(|error| invalid_json_selector("compile", error))?;
        }
    }
    Ok(())
}

fn is_fingerprint(value: &str) -> bool {
    value.len() == CONTENT_FINGERPRINT_HEX_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn decode_json_cursor(cursor: &str) -> Result<JsonCursor> {
    if cursor.len() > MAX_JSON_CURSOR_BYTES {
        return Err(Error::StaleCursor);
    }
    let mut fields = cursor.split(':');
    let version = fields.next();
    let source_hash = fields.next();
    let query_hash = fields.next();
    let offset = fields.next();
    if version != Some("j1") || fields.next().is_some() {
        return Err(Error::StaleCursor);
    }
    let (Some(source_hash), Some(query_hash), Some(offset)) = (source_hash, query_hash, offset)
    else {
        return Err(Error::StaleCursor);
    };
    if !is_fingerprint(source_hash) || !is_fingerprint(query_hash) {
        return Err(Error::StaleCursor);
    }
    let offset = offset.parse::<usize>().map_err(|_| Error::StaleCursor)?;
    if offset == 0 {
        return Err(Error::StaleCursor);
    }
    Ok(JsonCursor {
        source_hash: source_hash.to_owned(),
        query_hash: query_hash.to_owned(),
        offset,
    })
}

fn json_query_hash(operation: &JsonOperation) -> Result<String> {
    let serialized = serde_json::to_string(operation)
        .map_err(|error| Error::InternalFailure(error.to_string()))?;
    Ok(crate::text::hash(&serialized))
}

fn make_json_cursor(source_hash: &str, query_hash: &str, offset: usize) -> String {
    format!("j1:{source_hash}:{query_hash}:{offset}")
}

fn validate_json_request(request: &JsonRequest) -> Result<()> {
    match &request.operation {
        JsonOperation::Query { path, selector, .. }
        | JsonOperation::NumericSummary { path, selector } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            if let Some(selector) = selector {
                validate_selector(selector)?;
            }
        }
        JsonOperation::DiffFields {
            base_path,
            head_path,
            selectors,
            ..
        } => {
            validate_input(base_path, "base path", MAX_PATH_BYTES)?;
            validate_input(head_path, "head path", MAX_PATH_BYTES)?;
            validate_positive_request_limit("selectors", selectors.len(), MAX_JSON_SELECTORS)?;
            for selector in selectors {
                validate_selector(selector)?;
            }
        }
    }
    if let Some(max_items) = request.max_items {
        validate_positive_request_limit("max_items", max_items, MAX_JSON_ITEMS)?;
    }
    if let Some(array_sample_size) = request.array_sample_size {
        validate_request_limit(
            "array_sample_size",
            array_sample_size,
            MAX_ARRAY_SAMPLE_SIZE,
        )?;
    }
    if let Some(cursor) = request.cursor.as_deref() {
        decode_json_cursor(cursor)?;
        if !matches!(
            &request.operation,
            JsonOperation::Query {
                projection: JsonProjection::Keys,
                ..
            }
        ) {
            return Err(Error::InvalidInput {
                field: "cursor",
                reason: "is supported only for query operations with the keys projection",
            });
        }
    }
    Ok(())
}

impl Services {
    /// Query, summarize, or compare bounded live JSON structures.
    pub async fn json(&self, request: JsonRequest) -> Result<JsonResponse> {
        self.json_cancellable(request, CancellationToken::new())
            .await
    }

    /// Query live JSON structures with cooperative cancellation.
    pub async fn json_cancellable(
        &self,
        request: JsonRequest,
        cancellation: CancellationToken,
    ) -> Result<JsonResponse> {
        validate_json_request(&request)?;
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.json_sync(request, &cancellation)).await?
    }

    fn json_sync(
        &self,
        request: JsonRequest,
        cancellation: &CancellationToken,
    ) -> Result<JsonResponse> {
        check_cancelled(cancellation)?;
        validate_json_request(&request)?;
        let max_tokens = self.token_limit(request.max_tokens, DEFAULT_JSON_TOKENS)?;
        let max_items = request.max_items.unwrap_or(DEFAULT_JSON_ITEMS);
        let array_sample_size = request
            .array_sample_size
            .unwrap_or(DEFAULT_ARRAY_SAMPLE_SIZE);
        let cursor = request
            .cursor
            .as_deref()
            .map(decode_json_cursor)
            .transpose()?;
        let query_hash = json_query_hash(&request.operation)?;
        let generation = self.storage.repository_generation()?;
        let mut projected_tokens = 0usize;
        let baseline_source_tokens;
        let mut response = match request.operation {
            JsonOperation::Query {
                path,
                selector,
                projection,
            } => {
                let loaded = self.load_json(&path)?;
                baseline_source_tokens = loaded.source_tokens;
                let selected = select_json(&loaded.value, selector.as_ref())?;
                let value = selected.value.ok_or(Error::InvalidInput {
                    field: "selector",
                    reason: "did not match a JSON value",
                })?;
                if projection == JsonProjection::Keys {
                    let page = project_key_page(
                        self,
                        &value,
                        max_items,
                        max_tokens,
                        cursor.as_ref(),
                        &loaded.source.content_hash,
                        &query_hash,
                    )?;
                    projected_tokens = page.projected_tokens;
                    JsonResponse {
                        kind: "query".into(),
                        value: Some(page.value),
                        numeric_summary: None,
                        differences: Vec::new(),
                        sources: vec![loaded.source],
                        result_complete: page.remaining_items == 0,
                        total_items: Some(page.total_items),
                        returned_items: Some(page.returned_items),
                        remaining_items: Some(page.remaining_items),
                        incomplete_reason: page.incomplete_reason,
                        meta: self.meta(generation, 0, page.next_cursor),
                    }
                } else {
                    let total_items = projection_item_count(&value, projection, array_sample_size);
                    let mut state = ProjectionState {
                        remaining: max_items,
                        array_sample_size,
                        complete: true,
                    };
                    let value = project_json(&value, projection, &mut state)?;
                    projected_tokens = json_tokens(self, &value)?;
                    let returned_items = total_items.min(max_items);
                    let remaining_items = total_items.saturating_sub(returned_items);
                    JsonResponse {
                        kind: "query".into(),
                        value: Some(value),
                        numeric_summary: None,
                        differences: Vec::new(),
                        sources: vec![loaded.source],
                        result_complete: state.complete,
                        total_items: (!state.complete).then_some(total_items),
                        returned_items: (!state.complete).then_some(returned_items),
                        remaining_items: (!state.complete).then_some(remaining_items),
                        incomplete_reason: (!state.complete)
                            .then_some(JsonIncompleteReason::MaxItems),
                        meta: self.meta(generation, 0, None),
                    }
                }
            }
            JsonOperation::NumericSummary { path, selector } => {
                let loaded = self.load_json(&path)?;
                baseline_source_tokens = loaded.source_tokens;
                let selected = select_json(&loaded.value, selector.as_ref())?;
                let value = selected.value.ok_or(Error::InvalidInput {
                    field: "selector",
                    reason: "did not match a JSON value",
                })?;
                JsonResponse {
                    kind: "numeric_summary".into(),
                    value: None,
                    numeric_summary: Some(numeric_summary(&value)),
                    differences: Vec::new(),
                    sources: vec![loaded.source],
                    result_complete: true,
                    total_items: None,
                    returned_items: None,
                    remaining_items: None,
                    incomplete_reason: None,
                    meta: self.meta(generation, 0, None),
                }
            }
            JsonOperation::DiffFields {
                base_path,
                head_path,
                selectors,
                projection,
            } => {
                let before = self.load_json(&base_path)?;
                check_cancelled(cancellation)?;
                let after = self.load_json(&head_path)?;
                baseline_source_tokens = before.source_tokens.saturating_add(after.source_tokens);
                let mut state = ProjectionState {
                    remaining: max_items,
                    array_sample_size,
                    complete: true,
                };
                let mut differences = Vec::with_capacity(selectors.len());
                let mut total_items = 0usize;
                for selector in selectors {
                    check_cancelled(cancellation)?;
                    let before_selected = select_json(&before.value, Some(&selector))?;
                    let after_selected = select_json(&after.value, Some(&selector))?;
                    let changed = before_selected.present != after_selected.present
                        || before_selected.value != after_selected.value;
                    total_items = total_items.saturating_add(
                        before_selected
                            .value
                            .as_ref()
                            .map(|value| {
                                projection_item_count(value, projection, array_sample_size)
                            })
                            .unwrap_or_default(),
                    );
                    total_items = total_items.saturating_add(
                        after_selected
                            .value
                            .as_ref()
                            .map(|value| {
                                projection_item_count(value, projection, array_sample_size)
                            })
                            .unwrap_or_default(),
                    );
                    let before_value = before_selected
                        .value
                        .as_ref()
                        .map(|value| project_json(value, projection, &mut state))
                        .transpose()?;
                    let after_value = after_selected
                        .value
                        .as_ref()
                        .map(|value| project_json(value, projection, &mut state))
                        .transpose()?;
                    if let Some(value) = &before_value {
                        projected_tokens =
                            projected_tokens.saturating_add(json_tokens(self, value)?);
                    }
                    if let Some(value) = &after_value {
                        projected_tokens =
                            projected_tokens.saturating_add(json_tokens(self, value)?);
                    }
                    differences.push(JsonFieldDiff {
                        selector,
                        before_present: before_selected.present,
                        before: before_value,
                        after_present: after_selected.present,
                        after: after_value,
                        changed,
                    });
                }
                let returned_items = total_items.min(max_items);
                let remaining_items = total_items.saturating_sub(returned_items);
                JsonResponse {
                    kind: "diff_fields".into(),
                    value: None,
                    numeric_summary: None,
                    differences,
                    sources: vec![before.source, after.source],
                    result_complete: state.complete,
                    total_items: (!state.complete).then_some(total_items),
                    returned_items: (!state.complete).then_some(returned_items),
                    remaining_items: (!state.complete).then_some(remaining_items),
                    incomplete_reason: (!state.complete).then_some(JsonIncompleteReason::MaxItems),
                    meta: self.meta(generation, 0, None),
                }
            }
        };
        if projected_tokens > max_tokens {
            return Err(Error::RequestLimitExceeded {
                field: "projected JSON tokens",
                requested: projected_tokens,
                limit: max_tokens,
            });
        }
        response.meta.source_tokens = projected_tokens;
        response.meta.emitted_tokens = projected_tokens;
        self.finalize_response(&mut response)?;
        self.record_token_savings(
            TokenAccountingOperation::Json,
            Some(baseline_source_tokens),
            &response.meta,
        );
        Ok(response)
    }

    fn load_json(&self, path: &str) -> Result<LoadedJson> {
        let path = normalize_relative(path)?;
        let mut file = open_live_file(self, &path)?;
        let max_bytes = usize::try_from(self.config.max_file_bytes).unwrap_or(usize::MAX);
        let metadata_bytes = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        if metadata_bytes > max_bytes {
            return Err(Error::RequestLimitExceeded {
                field: "JSON file bytes",
                requested: metadata_bytes,
                limit: max_bytes,
            });
        }
        let mut bytes = Vec::with_capacity(metadata_bytes.min(max_bytes));
        file.by_ref()
            .take(
                u64::try_from(max_bytes)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(Error::RequestLimitExceeded {
                field: "JSON file bytes",
                requested: bytes.len(),
                limit: max_bytes,
            });
        }
        let content = std::str::from_utf8(&bytes).map_err(|_| Error::InvalidInput {
            field: "path",
            reason: "JSON file is not valid UTF-8",
        })?;
        let value = serde_json::from_str(content).map_err(|error| {
            let syntax_category = match error.classify() {
                serde_json::error::Category::Io => "io",
                serde_json::error::Category::Syntax => "syntax",
                serde_json::error::Category::Data => "data",
                serde_json::error::Category::Eof => "eof",
            };
            Error::InvalidJson {
                syntax_category,
                byte_offset: json_error_byte_offset(content, error.line(), error.column()),
                line: error.line(),
                column: error.column(),
                reason: error.to_string(),
            }
        })?;
        Ok(LoadedJson {
            source: JsonSource {
                path,
                content_hash: crate::text::hash(content),
                bytes: bytes.len(),
            },
            value,
            source_tokens: self.config.tokenizer.count(content),
        })
    }
}

fn select_json(value: &Value, selector: Option<&JsonSelector>) -> Result<SelectedJson> {
    match selector {
        None => Ok(SelectedJson {
            present: true,
            value: Some(value.clone()),
        }),
        Some(JsonSelector::Pointer { pointer }) => {
            let selected = value.pointer(pointer).cloned();
            Ok(SelectedJson {
                present: selected.is_some(),
                value: selected,
            })
        }
        Some(JsonSelector::Jmespath { expression }) => {
            let expression = jmespath::compile(expression)
                .map_err(|error| invalid_json_selector("compile", error))?;
            let selected = expression
                .search(value)
                .map_err(|error| invalid_json_selector("evaluate", error))?;
            let selected = serde_json::to_value(selected.as_ref())
                .map_err(|error| Error::InternalFailure(error.to_string()))?;
            Ok(SelectedJson {
                present: true,
                value: Some(selected),
            })
        }
    }
}

fn json_error_byte_offset(content: &str, line: usize, column: usize) -> usize {
    if line == 0 {
        return 0;
    }
    let mut line_start = 0usize;
    for (index, segment) in content.split_inclusive('\n').enumerate() {
        if index.saturating_add(1) == line {
            return line_start
                .saturating_add(column.saturating_sub(1).min(segment.len()))
                .min(content.len());
        }
        line_start = line_start.saturating_add(segment.len());
    }
    content.len()
}

fn collect_all_keys(value: &Value, pointer: &str, keys: &mut BTreeMap<String, &'static str>) {
    if !keys.contains_key(pointer) {
        keys.insert(pointer.to_owned(), json_type(value));
    }
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                let pointer = format!("{pointer}/{}", escape_pointer(key));
                collect_all_keys(value, &pointer, keys);
            }
        }
        Value::Array(values) => {
            let pointer = format!("{pointer}/*");
            for value in values {
                collect_all_keys(value, &pointer, keys);
            }
        }
        _ => {}
    }
}

fn key_entries(value: &Value) -> Vec<Value> {
    let mut keys = BTreeMap::new();
    collect_all_keys(value, "", &mut keys);
    keys.into_iter()
        .map(|(pointer, value_type)| json!({"pointer": pointer, "type": value_type}))
        .collect()
}

fn key_prefix_tokens(services: &Services, entries: &[Value], length: usize) -> Result<usize> {
    json_tokens(services, &Value::Array(entries[..length].to_vec()))
}

fn largest_key_prefix_within_tokens(
    services: &Services,
    entries: &[Value],
    max_tokens: usize,
) -> Result<(usize, usize)> {
    if entries.is_empty() {
        return Ok((0, json_tokens(services, &Value::Array(Vec::new()))?));
    }
    let full_tokens = key_prefix_tokens(services, entries, entries.len())?;
    if full_tokens <= max_tokens {
        return Ok((entries.len(), full_tokens));
    }

    let mut lower = 0usize;
    let mut upper = entries.len();
    while lower < upper {
        let middle = lower.saturating_add(upper).saturating_add(1) / 2;
        if key_prefix_tokens(services, entries, middle)? <= max_tokens {
            lower = middle;
        } else {
            upper = middle.saturating_sub(1);
        }
    }
    let tokens = key_prefix_tokens(services, entries, lower)?;
    Ok((lower, tokens))
}

fn project_key_page(
    services: &Services,
    value: &Value,
    max_items: usize,
    max_tokens: usize,
    cursor: Option<&JsonCursor>,
    source_hash: &str,
    query_hash: &str,
) -> Result<KeyProjectionPage> {
    let entries = key_entries(value);
    let total_items = entries.len();
    let offset = match cursor {
        Some(cursor) if cursor.source_hash == source_hash && cursor.query_hash == query_hash => {
            cursor.offset
        }
        Some(_) => return Err(Error::StaleCursor),
        None => 0,
    };
    if offset > total_items || (offset == total_items && offset != 0) {
        return Err(Error::StaleCursor);
    }

    let page_end = offset.saturating_add(max_items).min(total_items);
    let candidates = &entries[offset..page_end];
    let (returned_items, projected_tokens) =
        largest_key_prefix_within_tokens(services, candidates, max_tokens)?;
    if returned_items == 0 && !candidates.is_empty() {
        return Err(Error::RequestLimitExceeded {
            field: "one projected JSON item tokens",
            requested: key_prefix_tokens(services, candidates, 1)?,
            limit: max_tokens,
        });
    }

    let consumed = offset.saturating_add(returned_items);
    let remaining_items = total_items.saturating_sub(consumed);
    let incomplete_reason = (remaining_items > 0).then_some(if returned_items < candidates.len() {
        JsonIncompleteReason::MaxTokens
    } else {
        JsonIncompleteReason::MaxItems
    });
    let next_cursor =
        (remaining_items > 0).then(|| make_json_cursor(source_hash, query_hash, consumed));
    Ok(KeyProjectionPage {
        value: Value::Array(candidates[..returned_items].to_vec()),
        total_items,
        returned_items,
        remaining_items,
        incomplete_reason,
        next_cursor,
        projected_tokens,
    })
}

fn collapsed_item_count(value: &Value, array_sample_size: usize) -> usize {
    match value {
        Value::Array(values) => 1usize.saturating_add(
            values
                .iter()
                .take(array_sample_size)
                .map(|value| collapsed_item_count(value, array_sample_size))
                .fold(0usize, usize::saturating_add),
        ),
        Value::Object(values) => 1usize.saturating_add(
            values
                .values()
                .map(|value| collapsed_item_count(value, array_sample_size))
                .fold(0usize, usize::saturating_add),
        ),
        _ => 1,
    }
}

fn projection_item_count(
    value: &Value,
    projection: JsonProjection,
    array_sample_size: usize,
) -> usize {
    match projection {
        JsonProjection::Value | JsonProjection::Schema => count_nodes(value),
        JsonProjection::Collapsed => collapsed_item_count(value, array_sample_size),
        JsonProjection::Keys => key_entries(value).len(),
    }
}

fn project_json(
    value: &Value,
    projection: JsonProjection,
    state: &mut ProjectionState,
) -> Result<Value> {
    match projection {
        JsonProjection::Value => {
            let items = count_nodes(value);
            if items > state.remaining {
                return Err(Error::RequestLimitExceeded {
                    field: "selected JSON items",
                    requested: items,
                    limit: state.remaining,
                });
            }
            state.remaining -= items;
            Ok(value.clone())
        }
        JsonProjection::Collapsed => Ok(collapse_json(value, state)),
        JsonProjection::Keys => {
            let mut keys = BTreeMap::new();
            collect_keys(value, "", &mut keys, state);
            Ok(Value::Array(
                keys.into_iter()
                    .map(|(pointer, value_type)| json!({"pointer": pointer, "type": value_type}))
                    .collect(),
            ))
        }
        JsonProjection::Schema => Ok(infer_schema(value, state)),
    }
}

fn count_nodes(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1usize.saturating_add(
            values
                .iter()
                .map(count_nodes)
                .fold(0usize, usize::saturating_add),
        ),
        Value::Object(values) => 1usize.saturating_add(
            values
                .values()
                .map(count_nodes)
                .fold(0usize, usize::saturating_add),
        ),
        _ => 1,
    }
}

fn take_item(state: &mut ProjectionState) -> bool {
    if state.remaining == 0 {
        state.complete = false;
        return false;
    }
    state.remaining -= 1;
    true
}

fn collapse_json(value: &Value, state: &mut ProjectionState) -> Value {
    if !take_item(state) {
        return Value::Null;
    }
    match value {
        Value::Array(values) => {
            let mut sample = Vec::new();
            for value in values.iter().take(state.array_sample_size) {
                if state.remaining == 0 {
                    state.complete = false;
                    break;
                }
                sample.push(collapse_json(value, state));
            }
            let omitted = values.len().saturating_sub(sample.len());
            json!({
                "$array": {
                    "count": values.len(),
                    "sample": sample,
                    "omitted": omitted,
                }
            })
        }
        Value::Object(values) => {
            let mut projected = Map::new();
            for (key, value) in values {
                if state.remaining == 0 {
                    state.complete = false;
                    break;
                }
                projected.insert(key.clone(), collapse_json(value, state));
            }
            Value::Object(projected)
        }
        _ => value.clone(),
    }
}

fn collect_keys(
    value: &Value,
    pointer: &str,
    keys: &mut BTreeMap<String, &'static str>,
    state: &mut ProjectionState,
) {
    if !keys.contains_key(pointer) {
        if !take_item(state) {
            return;
        }
        keys.insert(pointer.to_owned(), json_type(value));
    }
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                let pointer = format!("{pointer}/{}", escape_pointer(key));
                collect_keys(value, &pointer, keys, state);
            }
        }
        Value::Array(values) => {
            let pointer = format!("{pointer}/*");
            for value in values {
                collect_keys(value, &pointer, keys, state);
            }
        }
        _ => {}
    }
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn infer_schema(value: &Value, state: &mut ProjectionState) -> Value {
    if !take_item(state) {
        return json!({"type": "unknown"});
    }
    match value {
        Value::Object(values) => {
            let mut properties = Map::new();
            for (key, value) in values {
                if state.remaining == 0 {
                    state.complete = false;
                    break;
                }
                properties.insert(key.clone(), infer_schema(value, state));
            }
            json!({"type": "object", "properties": properties})
        }
        Value::Array(values) => {
            let mut variants = BTreeMap::new();
            for value in values {
                if state.remaining == 0 {
                    state.complete = false;
                    break;
                }
                let schema = infer_schema(value, state);
                let key = serde_json::to_string(&schema).unwrap_or_default();
                variants.entry(key).or_insert(schema);
            }
            let items = match variants.len() {
                0 => json!({}),
                1 => variants.into_values().next().unwrap_or_else(|| json!({})),
                _ => json!({"anyOf": variants.into_values().collect::<Vec<_>>() }),
            };
            json!({"type": "array", "count": values.len(), "items": items})
        }
        _ => json!({"type": json_type(value)}),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn numeric_summary(value: &Value) -> JsonNumericSummary {
    let mut values = Vec::new();
    let mut non_numeric_count = 0usize;
    collect_numbers(value, &mut values, &mut non_numeric_count);
    values.sort_by(f64::total_cmp);
    let count = values.len();
    let median = match count {
        0 => None,
        count if count % 2 == 1 => Some(values[count / 2]),
        count => Some((values[count / 2 - 1] + values[count / 2]) / 2.0),
    };
    let p95 = (count > 0).then(|| {
        let rank = (count.saturating_mul(95).saturating_add(99)) / 100;
        values[rank.saturating_sub(1).min(count - 1)]
    });
    JsonNumericSummary {
        count,
        non_numeric_count,
        min: values.first().copied(),
        median,
        p95,
        max: values.last().copied(),
    }
}

fn collect_numbers(value: &Value, values: &mut Vec<f64>, non_numeric_count: &mut usize) {
    match value {
        Value::Number(value) => {
            if let Some(value) = value.as_f64() {
                values.push(value);
            } else {
                *non_numeric_count = non_numeric_count.saturating_add(1);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_numbers(item, values, non_numeric_count);
            }
        }
        Value::Object(items) => {
            for item in items.values() {
                collect_numbers(item, values, non_numeric_count);
            }
        }
        _ => *non_numeric_count = non_numeric_count.saturating_add(1),
    }
}

fn json_tokens(services: &Services, value: &Value) -> Result<usize> {
    let serialized =
        serde_json::to_string(value).map_err(|error| Error::InternalFailure(error.to_string()))?;
    Ok(services.config.tokenizer.count(&serialized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_projection_deduplicates_homogeneous_array_paths_before_item_caps() {
        let value = json!([
            {"score": 1, "name": "a"},
            {"score": 2, "name": "b"},
            {"score": 3, "name": "c"}
        ]);
        let mut state = ProjectionState {
            remaining: 4,
            array_sample_size: 3,
            complete: true,
        };
        let projected =
            project_json(&value, JsonProjection::Keys, &mut state).expect("keys projection");

        assert!(state.complete);
        assert_eq!(projected.as_array().map(Vec::len), Some(4));
    }

    #[test]
    fn keys_projection_detects_late_heterogeneous_paths_after_the_item_cap() {
        let value = json!([{"first": 1}, {"second": 2}]);
        let mut state = ProjectionState {
            remaining: 3,
            array_sample_size: 3,
            complete: true,
        };
        let projected =
            project_json(&value, JsonProjection::Keys, &mut state).expect("keys projection");

        assert!(!state.complete);
        assert_eq!(projected.as_array().map(Vec::len), Some(3));
        assert_eq!(key_entries(&value).len(), 4);
    }

    #[test]
    fn collapsed_projection_reports_actual_bounded_sample() {
        let value = json!([1, 2, 3, 4]);
        let mut state = ProjectionState {
            remaining: 2,
            array_sample_size: 3,
            complete: true,
        };
        let projected = collapse_json(&value, &mut state);

        assert!(!state.complete);
        assert_eq!(
            projected["$array"]["sample"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(projected["$array"]["omitted"], 3);
    }
}
