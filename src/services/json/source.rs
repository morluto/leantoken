//! Loading live JSON files into bounded in-memory values and token accounting.

use std::io::Read;

use serde_json::Value;

use crate::model::JsonSource;
use crate::repository::normalize_relative;
use crate::services::Services;
use crate::services::read::open_live_file;
use crate::{Error, Result};

pub(super) struct LoadedJson {
    source: JsonSource,
    value: Value,
    source_tokens: usize,
}

impl LoadedJson {
    pub(super) fn value(&self) -> &Value {
        &self.value
    }

    pub(super) fn source(&self) -> &JsonSource {
        &self.source
    }

    pub(super) fn into_source(self) -> JsonSource {
        self.source
    }

    pub(super) fn source_tokens(&self) -> usize {
        self.source_tokens
    }
}

pub(super) fn json_tokens(services: &Services, value: &Value) -> Result<usize> {
    let serialized = serde_json::to_string(value)
        .map_err(|error| Error::SerializationFailure(error.to_string()))?;
    Ok(services.config.tokenizer.count(&serialized))
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

impl Services {
    pub(super) fn load_json(&self, path: &str) -> Result<LoadedJson> {
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
