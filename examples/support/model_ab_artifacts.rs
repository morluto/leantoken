use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ARTIFACT_SCHEMA_V1: u32 = 1;
pub const TOOL_TRACE_FILE: &str = "tool-trace.json";
pub const TRAJECTORY_FILE: &str = "trajectory.json";
pub const PROVIDER_USAGE_FILE: &str = "provider-usage.json";
pub const PREWALK_HANDOFF_FILE: &str = "prewalk-handoff.json";
pub const ORIENTATION_CAPSULE_MAX_PATHS: usize = 1;
pub const ORIENTATION_CAPSULE_MAX_TERMS: usize = 4;
pub const ORIENTATION_CAPSULE_MAX_DEFINITIONS: usize = 4;
pub const ORIENTATION_CAPSULE_MAX_TOKENS: usize = 128;

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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OrientationCapsule {
    pub entries: Vec<OrientationCapsuleEntry>,
    pub capsule_tokens: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OrientationCapsuleEntry {
    pub path: String,
    pub matched_terms: Vec<String>,
    pub definitions: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation_capsule: Option<OrientationCapsule>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ValidatedEdit {
    pub edit_sequence: usize,
    pub validation_sequence: usize,
}

pub fn validate_orientation_capsule(
    capsule: &OrientationCapsule,
    tokenizer: leantoken::tokens::Tokenizer,
) -> Result<(), String> {
    if capsule.entries.is_empty() || capsule.entries.len() > ORIENTATION_CAPSULE_MAX_PATHS {
        return Err("orientation capsule must contain exactly one bounded path".to_owned());
    }
    for entry in &capsule.entries {
        if !is_relative_capsule_path(&entry.path)
            || entry.matched_terms.is_empty()
            || entry.matched_terms.len() > ORIENTATION_CAPSULE_MAX_TERMS
            || entry.definitions.len() > ORIENTATION_CAPSULE_MAX_DEFINITIONS
            || entry
                .matched_terms
                .iter()
                .chain(&entry.definitions)
                .any(|value| value.trim().is_empty())
        {
            return Err("orientation capsule entry exceeds its structural bounds".to_owned());
        }
    }
    let serialized = serde_json::to_string(&capsule.entries)
        .map_err(|error| format!("orientation capsule serialization failed: {error}"))?;
    let exact_tokens = tokenizer.count(&serialized);
    if exact_tokens != capsule.capsule_tokens || exact_tokens > ORIENTATION_CAPSULE_MAX_TOKENS {
        return Err(format!(
            "orientation capsule token count is {}, expected {}, with maximum {}",
            capsule.capsule_tokens, exact_tokens, ORIENTATION_CAPSULE_MAX_TOKENS
        ));
    }
    Ok(())
}

#[allow(dead_code)]
pub fn orientation_capsule_prompt(
    capsule: &OrientationCapsule,
) -> Result<String, serde_json::Error> {
    Ok(format!(
        "\n\nBounded orientation capsule (routing hint, not source evidence):\n\
         Inspect the named owner first, verify it with repository evidence before editing, and \
         record contrary evidence instead of forcing the route.\n\
         ORIENTATION_CAPSULE_JSON\n{}\nEND_ORIENTATION_CAPSULE_JSON",
        serde_json::to_string(capsule)?
    ))
}

fn is_relative_capsule_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

pub fn is_bounded_prewalk_todo_event(event: &Value) -> bool {
    if event["type"].as_str() != Some("item.completed")
        || event.pointer("/item/type").and_then(Value::as_str) != Some("agent_message")
    {
        return false;
    }
    let Some(text) = event.pointer("/item/text").and_then(Value::as_str) else {
        return false;
    };
    let Ok(response) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let Some(response) = response.as_object() else {
        return false;
    };
    if response.len() != 2
        || response
            .get("summary")
            .and_then(Value::as_str)
            .is_none_or(|summary| summary.trim().is_empty())
    {
        return false;
    }
    let Some(todo) = response.get("todo").and_then(Value::as_array) else {
        return false;
    };
    !todo.is_empty()
        && todo.len() <= 8
        && todo.iter().all(|item| {
            item.as_object().is_some_and(|item| {
                item.len() == 2
                    && item
                        .get("step")
                        .and_then(Value::as_str)
                        .is_some_and(|step| !step.trim().is_empty())
                    && matches!(
                        item.get("status").and_then(Value::as_str),
                        Some("pending" | "in_progress" | "completed")
                    )
            })
        })
}
