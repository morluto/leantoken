use std::collections::BTreeMap;
use std::io::Read;

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::read::open_live_file;
use super::validation::{MAX_PATH_BYTES, MAX_PATTERN_BYTES, check_cancelled, validate_input};
use super::{Services, validate_positive_request_limit, validate_request_limit};
use crate::model::{
    JsonFieldDiff, JsonNumericSummary, JsonOperation, JsonProjection, JsonRequest, JsonResponse,
    JsonSelector, JsonSource, TokenAccountingOperation,
};
use crate::repository::normalize_relative;
use crate::{Error, Result};

const DEFAULT_JSON_TOKENS: usize = 8_000;
const DEFAULT_JSON_ITEMS: usize = 1_000;
const MAX_JSON_ITEMS: usize = 10_000;
const DEFAULT_ARRAY_SAMPLE_SIZE: usize = 3;
const MAX_ARRAY_SAMPLE_SIZE: usize = 20;
const MAX_JSON_SELECTORS: usize = 100;

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
            jmespath::compile(expression).map_err(|_| Error::InvalidInput {
                field: "JMESPath expression",
                reason: "could not be compiled",
            })?;
        }
    }
    Ok(())
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
                let mut state = ProjectionState {
                    remaining: max_items,
                    array_sample_size,
                    complete: true,
                };
                let value = project_json(&value, projection, &mut state)?;
                projected_tokens = json_tokens(self, &value)?;
                JsonResponse {
                    kind: "query".into(),
                    value: Some(value),
                    numeric_summary: None,
                    differences: Vec::new(),
                    sources: vec![loaded.source],
                    result_complete: state.complete,
                    meta: self.meta(generation, 0, None),
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
                for selector in selectors {
                    check_cancelled(cancellation)?;
                    let before_selected = select_json(&before.value, Some(&selector))?;
                    let after_selected = select_json(&after.value, Some(&selector))?;
                    let changed = before_selected.present != after_selected.present
                        || before_selected.value != after_selected.value;
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
                JsonResponse {
                    kind: "diff_fields".into(),
                    value: None,
                    numeric_summary: None,
                    differences,
                    sources: vec![before.source, after.source],
                    result_complete: state.complete,
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
        let value = serde_json::from_str(content).map_err(|_| Error::InvalidInput {
            field: "path",
            reason: "file is not valid JSON",
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
            let expression = jmespath::compile(expression).map_err(|_| Error::InvalidInput {
                field: "JMESPath expression",
                reason: "could not be compiled",
            })?;
            let selected = expression.search(value).map_err(|_| Error::InvalidInput {
                field: "JMESPath expression",
                reason: "could not be evaluated",
            })?;
            let selected = serde_json::to_value(selected.as_ref())
                .map_err(|error| Error::InternalFailure(error.to_string()))?;
            Ok(SelectedJson {
                present: true,
                value: Some(selected),
            })
        }
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
                if state.remaining == 0 {
                    break;
                }
            }
        }
        Value::Array(values) => {
            let pointer = format!("{pointer}/*");
            for value in values {
                collect_keys(value, &pointer, keys, state);
                if state.remaining == 0 {
                    break;
                }
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
