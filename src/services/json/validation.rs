//! Request-level validation for JSON operations, selectors, limits, depth, and
//! cursors.

use super::cursor::decode_json_cursor;
use super::execution::JsonExecutionOptions;
use super::{MAX_ARRAY_SAMPLE_SIZE, MAX_JSON_DEPTH, MAX_JSON_ITEMS, MAX_JSON_SELECTORS};
use crate::model::{JsonOperation, JsonProjection, JsonRequest, JsonSelector};
use crate::repository::validate_relative;
use crate::services::validation::{MAX_PATH_BYTES, MAX_PATTERN_BYTES, validate_input};
use crate::services::{validate_positive_request_limit, validate_request_limit};
use crate::{Error, Result};

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

pub(super) fn validate_json_request(
    request: &JsonRequest,
    execution: JsonExecutionOptions,
) -> Result<()> {
    match &request.operation {
        JsonOperation::Query { path, selector, .. }
        | JsonOperation::NumericSummary { path, selector } => {
            validate_input(path, "path", MAX_PATH_BYTES)?;
            validate_relative(path)?;
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
            validate_relative(base_path)?;
            validate_relative(head_path)?;
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
    if let Some(depth) = execution.depth() {
        validate_request_limit("depth", depth, MAX_JSON_DEPTH)?;
        if !matches!(
            &request.operation,
            JsonOperation::Query {
                projection: JsonProjection::Keys,
                ..
            }
        ) {
            return Err(Error::InvalidInput {
                field: "depth",
                reason: "is supported only for query operations with the keys projection",
            });
        }
    }
    if let Some(cursor) = request.cursor.as_deref() {
        decode_json_cursor(cursor, execution.cursor_version())?;
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
