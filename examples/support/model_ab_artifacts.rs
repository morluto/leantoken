use serde::{Deserialize, Serialize};

pub const ARTIFACT_SCHEMA_V1: u32 = 1;
pub const TOOL_TRACE_FILE: &str = "tool-trace.json";
pub const TRAJECTORY_FILE: &str = "trajectory.json";
pub const PROVIDER_USAGE_FILE: &str = "provider-usage.json";
pub const PREWALK_HANDOFF_FILE: &str = "prewalk-handoff.json";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunBinding {
    pub experiment_id: String,
    pub manifest_blake3: String,
    pub task_id: String,
    pub repetition: usize,
    pub arm: String,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolTrace {
    pub schema_version: u32,
    pub binding: RunBinding,
    pub calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    pub sequence: usize,
    pub tool_name: String,
    pub call_id: String,
    pub result_id: String,
    pub outcome: ToolOutcome,
    pub result_source_tokens: u64,
    #[serde(default)]
    pub reread: bool,
    #[serde(default)]
    pub ranges: Vec<RangeIdentity>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Success,
    FailedSearch,
    DeadEndRead,
    Error,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RangeIdentity {
    pub repository_generation: u64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content_hash: String,
    #[serde(default)]
    pub source_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Trajectory {
    pub schema_version: u32,
    pub binding: RunBinding,
    pub events: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderUsageReceipt {
    pub schema_version: u32,
    pub binding: RunBinding,
    pub usage: ProviderUsage,
    pub raw_receipt: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrewalkHandoff {
    pub schema_version: u32,
    pub binding: RunBinding,
    pub primary_model: String,
    pub executor_model: String,
    pub trajectory_events: Vec<serde_json::Value>,
    pub todo_events: Vec<serde_json::Value>,
    pub evidence_calls: Vec<ToolCall>,
    pub worktree_patch: String,
    pub first_validated_edit: ValidatedEdit,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ValidatedEdit {
    pub edit_sequence: usize,
    pub validation_sequence: usize,
}
