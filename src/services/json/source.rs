//! Loading live JSON files into bounded in-memory values and token accounting.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum JsonMeasurementKey {
    /// Prefix length within the request-local keys candidate page.
    KeysPrefix(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JsonMeasurementCacheKey {
    KeysPrefix(usize, Value),
}

impl Hash for JsonMeasurementCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::KeysPrefix(length, value) => {
                length.hash(state);
                hash_json_value(value, state);
            }
        }
    }
}

fn hash_json_value<H: Hasher>(value: &Value, state: &mut H) {
    std::mem::discriminant(value).hash(state);
    match value {
        Value::Null => {}
        Value::Bool(value) => value.hash(state),
        Value::Number(value) => value.to_string().hash(state),
        Value::String(value) => value.hash(state),
        Value::Array(values) => values
            .iter()
            .for_each(|value| hash_json_value(value, state)),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            entries.into_iter().for_each(|(key, value)| {
                key.hash(state);
                hash_json_value(value, state);
            });
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct JsonMeasurementCounters {
    pub(super) serializations: usize,
    pub(super) tokenizer_counts: usize,
    pub(super) cache_hits: usize,
}

#[derive(Debug, Default)]
pub(super) struct JsonMeasurementCache {
    measurements: HashMap<JsonMeasurementCacheKey, (String, usize)>,
    counters: JsonMeasurementCounters,
}

impl JsonMeasurementCache {
    pub(super) fn measure(
        &mut self,
        services: &Services,
        key: JsonMeasurementKey,
        value: &Value,
    ) -> Result<usize> {
        let cache_key = match key {
            JsonMeasurementKey::KeysPrefix(length) => {
                JsonMeasurementCacheKey::KeysPrefix(length, value.clone())
            }
        };
        if let Some((_, tokens)) = self.measurements.get(&cache_key) {
            self.counters.cache_hits = self.counters.cache_hits.saturating_add(1);
            return Ok(*tokens);
        }
        let serialized = serde_json::to_string(value)
            .map_err(|error| Error::SerializationFailure(error.to_string()))?;
        self.counters.serializations = self.counters.serializations.saturating_add(1);
        let tokens = services.config.tokenizer.count(&serialized);
        self.counters.tokenizer_counts = self.counters.tokenizer_counts.saturating_add(1);
        self.measurements.insert(cache_key, (serialized, tokens));
        Ok(tokens)
    }

    #[cfg(test)]
    pub(super) fn counters(&self) -> &JsonMeasurementCounters {
        &self.counters
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
