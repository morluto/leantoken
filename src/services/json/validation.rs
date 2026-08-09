//! Request-level validation for JSON operations, selectors, limits, depth, and
//! cursors.

use super::cursor::{JsonCursor, decode_json_cursor, json_query_hash};
use super::execution::JsonExecutionOptions;
use super::selection::ParsedJsonSelector;
use super::{MAX_ARRAY_SAMPLE_SIZE, MAX_JSON_DEPTH, MAX_JSON_ITEMS, MAX_JSON_SELECTORS};
use crate::model::{JsonOperation, JsonProjection, JsonRequest};
use crate::repository::validate_relative;
use crate::services::validation::{MAX_PATH_BYTES, validate_input};
use crate::services::{validate_positive_request_limit, validate_request_limit};
use crate::{Error, Result};

pub(super) struct ParsedJsonRequest {
    pub(super) operation: ParsedJsonOperation,
    pub(super) max_tokens: Option<usize>,
    pub(super) max_items: Option<usize>,
    pub(super) array_sample_size: Option<usize>,
}

pub(super) enum ParsedJsonOperation {
    Query {
        path: String,
        selector: Option<ParsedJsonSelector>,
        projection: JsonProjection,
        cursor: Option<JsonCursor>,
        query_hash: String,
    },
    NumericSummary {
        path: String,
        selector: Option<ParsedJsonSelector>,
    },
    DiffFields {
        base_path: String,
        head_path: String,
        selectors: Vec<ParsedJsonSelector>,
        projection: JsonProjection,
    },
}

pub(super) fn parse_json_request(
    request: JsonRequest,
    execution: JsonExecutionOptions,
) -> Result<ParsedJsonRequest> {
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
    }

    let query_hash = json_query_hash(&request.operation, execution)?;
    let operation = match request.operation {
        JsonOperation::Query {
            path,
            selector,
            projection,
        } => {
            validate_input(&path, "path", MAX_PATH_BYTES)?;
            validate_relative(&path)?;
            if execution.depth().is_some() && projection != JsonProjection::Keys {
                return Err(Error::InvalidInput {
                    field: "depth",
                    reason: "is supported only for query operations with the keys projection",
                });
            }
            let cursor = request
                .cursor
                .as_deref()
                .map(|cursor| decode_json_cursor(cursor, execution.cursor_version()))
                .transpose()?;
            if cursor.is_some() && projection != JsonProjection::Keys {
                return Err(Error::InvalidInput {
                    field: "cursor",
                    reason: "is supported only for query operations with the keys projection",
                });
            }
            ParsedJsonOperation::Query {
                path,
                selector: selector.map(ParsedJsonSelector::parse).transpose()?,
                projection,
                cursor,
                query_hash,
            }
        }
        JsonOperation::NumericSummary { path, selector } => {
            validate_input(&path, "path", MAX_PATH_BYTES)?;
            validate_relative(&path)?;
            if execution.depth().is_some() {
                return Err(Error::InvalidInput {
                    field: "depth",
                    reason: "is supported only for query operations with the keys projection",
                });
            }
            if request.cursor.is_some() {
                return Err(Error::InvalidInput {
                    field: "cursor",
                    reason: "is supported only for query operations with the keys projection",
                });
            }
            ParsedJsonOperation::NumericSummary {
                path,
                selector: selector.map(ParsedJsonSelector::parse).transpose()?,
            }
        }
        JsonOperation::DiffFields {
            base_path,
            head_path,
            selectors,
            projection,
        } => {
            validate_input(&base_path, "base path", MAX_PATH_BYTES)?;
            validate_input(&head_path, "head path", MAX_PATH_BYTES)?;
            validate_relative(&base_path)?;
            validate_relative(&head_path)?;
            validate_positive_request_limit("selectors", selectors.len(), MAX_JSON_SELECTORS)?;
            if execution.depth().is_some() {
                return Err(Error::InvalidInput {
                    field: "depth",
                    reason: "is supported only for query operations with the keys projection",
                });
            }
            if request.cursor.is_some() {
                return Err(Error::InvalidInput {
                    field: "cursor",
                    reason: "is supported only for query operations with the keys projection",
                });
            }
            ParsedJsonOperation::DiffFields {
                base_path,
                head_path,
                selectors: selectors
                    .into_iter()
                    .map(ParsedJsonSelector::parse)
                    .collect::<Result<_>>()?,
                projection,
            }
        }
    };
    Ok(ParsedJsonRequest {
        operation,
        max_tokens: request.max_tokens,
        max_items: request.max_items,
        array_sample_size: request.array_sample_size,
    })
}
