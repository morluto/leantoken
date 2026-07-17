#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub const TRACE_SCHEMA_V1: u32 = 1;
pub const TRACE_SCHEMA_V2: u32 = 2;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Trace {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_content_blake3: Option<String>,
    pub host: String,
    pub host_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub tokenizer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count_exact: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at_unix_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usage: Option<ProviderUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_total_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<serde_json::Value>,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RepositoryIdentity {
    pub revision: String,
    pub dirty_fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    pub direction: Direction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_unix_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_visible_payload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<RangeIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_through_turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_prefix: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_eligible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Compaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usage: Option<ProviderUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ClientToServer,
    ServerToClient,
    Handoff,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RangeIdentity {
    pub repository_generation: u64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Compaction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_result_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderUsage {
    #[serde(default)]
    pub uncached_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
}

impl Trace {
    pub fn validate_version(&self) -> Result<(), String> {
        if !matches!(self.schema_version, TRACE_SCHEMA_V1 | TRACE_SCHEMA_V2) {
            return Err(format!(
                "unsupported wire trace schema version {}; expected 1 or 2",
                self.schema_version
            ));
        }
        if self.events.is_empty() {
            return Err("wire trace has no events".to_owned());
        }
        if self.schema_version == TRACE_SCHEMA_V2 {
            if self
                .trace_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err("schema v2 trace_id must be non-empty".to_owned());
            }
            if self.host.trim().is_empty()
                || self.host_version.trim().is_empty()
                || self.tokenizer.trim().is_empty()
            {
                return Err(
                    "schema v2 host, host_version, and tokenizer must be non-empty".to_owned(),
                );
            }
            if self.token_count_exact.is_none() {
                return Err("schema v2 token_count_exact must be present".to_owned());
            }
            if self.repository.as_ref().is_some_and(|repository| {
                repository.revision.trim().is_empty()
                    || repository.dirty_fingerprint.trim().is_empty()
            }) {
                return Err("schema v2 repository identity fields must be non-empty".to_owned());
            }
            if !self
                .events
                .iter()
                .enumerate()
                .all(|(index, event)| event.sequence == Some(index as u64))
            {
                return Err(
                    "schema v2 event sequence must be contiguous and start at zero".to_owned(),
                );
            }
            if self.events.iter().any(|event| {
                self.final_turn
                    .zip(event.turn)
                    .is_some_and(|(final_turn, turn)| turn > final_turn)
            }) {
                return Err("schema v2 event turn exceeds final_turn".to_owned());
            }
            if self
                .events
                .iter()
                .flat_map(|event| &event.ranges)
                .any(|range| {
                    range.path.trim().is_empty()
                        || range.start_line == 0
                        || range.end_line < range.start_line
                        || range.content_hash.trim().is_empty()
                })
            {
                return Err("schema v2 contains an invalid range identity".to_owned());
            }
            let expected = self
                .trace_content_blake3
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or("schema v2 trace_content_blake3 must be present")?;
            let actual = self.content_blake3()?;
            if expected != actual {
                return Err(format!(
                    "schema v2 trace content hash mismatch: expected {expected}, computed {actual}"
                ));
            }
        }
        Ok(())
    }

    pub fn seal_content_hash(&mut self) -> Result<(), String> {
        self.trace_content_blake3 = None;
        self.trace_content_blake3 = Some(self.content_blake3()?);
        Ok(())
    }

    pub fn content_blake3(&self) -> Result<String, String> {
        let mut unhashed = self.clone();
        unhashed.trace_content_blake3 = None;
        serde_json::to_vec(&unhashed)
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
            .map_err(|error| format!("failed to serialize trace for content hash: {error}"))
    }
}
