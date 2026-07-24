use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};

use clap::Parser;
use serde::Serialize;
use serde_json::Value;

type DynError = Box<dyn Error>;

const SCHEMA_VERSION: u32 = 1;
const MAX_RESPONSE_RANGES: usize = 2_048;

#[derive(Debug, Parser)]
#[command(about = "Create a source-free LeanToken trajectory fixture from a Codex rollout")]
struct Args {
    /// Private Codex rollout JSONL. Its path and contents are never copied to the output.
    #[arg(long)]
    input: PathBuf,
    /// New sanitized JSON fixture. Existing files are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Exclude events at or after this RFC 3339 timestamp.
    #[arg(long)]
    cutoff: Option<String>,
    /// Public source repository associated with the trajectory.
    #[arg(long)]
    repository: String,
    /// Public repository revision visible at the start of the workflow.
    #[arg(long)]
    base_revision: String,
    /// Public checkpoint in NAME=REVISION form. May be repeated.
    #[arg(long = "checkpoint", value_parser = parse_checkpoint)]
    checkpoints: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct SanitizedFixture {
    schema_version: u32,
    fixture_kind: &'static str,
    source: SourceReceipt,
    repository: RepositoryReceipt,
    controls: ControlReceipt,
    summary: TrajectorySummary,
    calls: Vec<RetrievalCall>,
    validations: Vec<ValidationCall>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct SourceReceipt {
    kind: &'static str,
    private_input_blake3: String,
    private_input_bytes: u64,
    sanitizer_source_blake3: String,
    raw_content_published: bool,
}

#[derive(Debug, Serialize)]
struct RepositoryReceipt {
    repository: String,
    base_revision: String,
    checkpoints: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ControlReceipt {
    cutoff: Option<String>,
    ordering: &'static str,
    query_text_retained: bool,
    task_text_retained: bool,
    source_content_retained: bool,
    session_identifiers_retained: bool,
    absolute_paths_retained: bool,
}

#[derive(Debug, Default, Serialize)]
struct TrajectorySummary {
    retrieval_calls: usize,
    calls_by_tool: BTreeMap<String, usize>,
    unique_request_paths: usize,
    requested_line_spans: u64,
    reads_longer_than_100_lines: usize,
    exact_read_repeats: usize,
    overlapping_nonidentical_reads: usize,
    expected_hash_inputs: usize,
    known_hash_inputs: usize,
    failed_calls: usize,
    unresolved_outcomes: usize,
    validation_calls: usize,
    successful_validations: usize,
    failed_validations: usize,
}

#[derive(Debug, Serialize)]
struct RetrievalCall {
    sequence: usize,
    tool: String,
    request: SanitizedRequest,
    outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<ResponseReceipt>,
}

#[derive(Debug, Default, Serialize)]
struct SanitizedRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_blake3: Option<String>,
    query_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_blake3: Option<String>,
    task_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    expected_hash_supplied: bool,
    known_hash_count: usize,
    consistency: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Success,
    Error,
    #[default]
    NotRecorded,
}

#[derive(Debug, Serialize)]
struct ResponseReceipt {
    output_blake3: String,
    output_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_class: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_stale: Option<bool>,
    returned_ranges: Vec<RangeReceipt>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct RangeReceipt {
    path: String,
    start_line: usize,
    end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct ValidationCall {
    sequence: usize,
    kinds: Vec<&'static str>,
    command_blake3: String,
    outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_blake3: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum PendingCall {
    Retrieval(usize),
    Validation(usize),
}

fn main() -> Result<(), DynError> {
    let args = Args::parse();
    validate_args(&args)?;
    let input = fs::File::open(&args.input)?;
    let mut reader = BufReader::new(input);
    let mut raw_hasher = blake3::Hasher::new();
    let mut raw_bytes = 0u64;
    let mut calls = Vec::new();
    let mut validations = Vec::new();
    let mut pending = HashMap::new();
    let mut event_sequence = 0usize;
    let mut line_number = 0usize;
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        line_number += 1;
        raw_hasher.update(&line);
        raw_bytes = raw_bytes.saturating_add(u64::try_from(read)?);
        let event: Value = serde_json::from_slice(trim_line_ending(&line))
            .map_err(|error| format!("invalid rollout JSON at line {line_number}: {error}"))?;
        if excluded_by_cutoff(&event, args.cutoff.as_deref()) {
            continue;
        }
        let Some(payload) = event.get("payload") else {
            continue;
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                event_sequence = event_sequence.saturating_add(1);
                let Some(name) = payload.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                let arguments = parse_arguments(payload.get("arguments"));
                if name.starts_with("leantoken_") {
                    let index = calls.len();
                    calls.push(RetrievalCall {
                        sequence: event_sequence,
                        tool: name.trim_start_matches("leantoken_").to_owned(),
                        request: sanitize_request(&arguments),
                        outcome: Outcome::NotRecorded,
                        response: None,
                    });
                    pending.insert(call_id.to_owned(), PendingCall::Retrieval(index));
                } else if name == "exec_command" {
                    let command = arguments
                        .get("cmd")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let kinds = validation_kinds(command);
                    if !kinds.is_empty() {
                        let index = validations.len();
                        validations.push(ValidationCall {
                            sequence: event_sequence,
                            kinds,
                            command_blake3: domain_hash("validation-command", command.as_bytes()),
                            outcome: Outcome::NotRecorded,
                            exit_code: None,
                            output_blake3: None,
                        });
                        pending.insert(call_id.to_owned(), PendingCall::Validation(index));
                    }
                }
            }
            Some("function_call_output") => {
                let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(owner) = pending.remove(call_id) else {
                    continue;
                };
                let output = payload.get("output").cloned().unwrap_or(Value::Null);
                match owner {
                    PendingCall::Retrieval(index) => {
                        let receipt = response_receipt(&output);
                        calls[index].outcome = if receipt.error_class.is_some() {
                            Outcome::Error
                        } else {
                            Outcome::Success
                        };
                        calls[index].response = Some(receipt);
                    }
                    PendingCall::Validation(index) => {
                        let bytes = value_bytes(&output);
                        let exit_code = find_i64(&parse_embedded_json(output.clone()), "exit_code");
                        validations[index].outcome = match exit_code {
                            Some(0) => Outcome::Success,
                            Some(_) => Outcome::Error,
                            None => Outcome::NotRecorded,
                        };
                        validations[index].exit_code = exit_code;
                        validations[index].output_blake3 =
                            Some(domain_hash("validation-output", &bytes));
                    }
                }
            }
            _ => {}
        }
    }

    let summary = summarize(&calls, &validations);
    let fixture = SanitizedFixture {
        schema_version: SCHEMA_VERSION,
        fixture_kind: "evolving_worktree_trajectory",
        source: SourceReceipt {
            kind: "private_codex_rollout",
            private_input_blake3: raw_hasher.finalize().to_hex().to_string(),
            private_input_bytes: raw_bytes,
            sanitizer_source_blake3: blake3::hash(include_bytes!("sanitize_codex_trajectory.rs"))
                .to_hex()
                .to_string(),
            raw_content_published: false,
        },
        repository: RepositoryReceipt {
            repository: args.repository,
            base_revision: args.base_revision,
            checkpoints: args.checkpoints.into_iter().collect(),
        },
        controls: ControlReceipt {
            cutoff: args.cutoff,
            ordering: "original function-call sequence",
            query_text_retained: false,
            task_text_retained: false,
            source_content_retained: false,
            session_identifiers_retained: false,
            absolute_paths_retained: false,
        },
        summary,
        calls,
        validations,
        limitations: vec![
            "The fixture preserves retrieval request shape, bounded response identity, ranges, and validation outcomes; it omits source, prompts, query text, command text, timestamps, and session identifiers.",
            "A response hash proves identity relative to the private rollout but does not make omitted response content independently reviewable.",
            "Command classification is allowlisted and may omit validation performed through an unrecognized wrapper.",
            "This observed trajectory is a scenario seed, not evidence that any intervention improves validated task success.",
        ],
    };
    write_new_json(&args.output, &fixture)
}

fn validate_args(args: &Args) -> Result<(), DynError> {
    if args.repository.trim().is_empty() {
        return Err("repository must not be empty".into());
    }
    if args.base_revision.trim().is_empty() {
        return Err("base revision must not be empty".into());
    }
    let mut names = BTreeSet::new();
    for (name, revision) in &args.checkpoints {
        if !names.insert(name) {
            return Err(format!("duplicate checkpoint name: {name}").into());
        }
        if revision.trim().is_empty() {
            return Err(format!("checkpoint revision is empty: {name}").into());
        }
    }
    Ok(())
}

fn parse_checkpoint(raw: &str) -> Result<(String, String), String> {
    let (name, revision) = raw
        .split_once('=')
        .ok_or_else(|| "checkpoint must use NAME=REVISION".to_owned())?;
    if name.trim().is_empty() || revision.trim().is_empty() {
        return Err("checkpoint name and revision must not be empty".to_owned());
    }
    Ok((name.to_owned(), revision.to_owned()))
}

fn excluded_by_cutoff(event: &Value, cutoff: Option<&str>) -> bool {
    let Some(cutoff) = cutoff else {
        return false;
    };
    event
        .get("timestamp")
        .and_then(Value::as_str)
        .is_some_and(|timestamp| timestamp >= cutoff)
}

fn trim_line_ending(mut bytes: &[u8]) -> &[u8] {
    if bytes.ends_with(b"\n") {
        bytes = &bytes[..bytes.len() - 1];
    }
    if bytes.ends_with(b"\r") {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn parse_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(arguments)) => {
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::Object(Default::default()))
        }
        Some(value) => value.clone(),
        None => Value::Object(Default::default()),
    }
}

fn sanitize_request(arguments: &Value) -> SanitizedRequest {
    let target = arguments.get("target").filter(|value| value.is_object());
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let task = arguments
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or_default();
    SanitizedRequest {
        operation: operation_name(arguments.get("operation")),
        path: arguments
            .get("path")
            .and_then(Value::as_str)
            .map(sanitize_path),
        paths: arguments
            .get("paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(sanitize_path)
            .collect(),
        target_kind: target
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        symbol: target
            .and_then(|value| value.get("name"))
            .or_else(|| arguments.get("symbol"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        start_line: target
            .and_then(|value| value.get("start"))
            .or_else(|| arguments.get("start_line"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        end_line: target
            .and_then(|value| value.get("end"))
            .or_else(|| arguments.get("end_line"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        mode: arguments
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_owned),
        query_blake3: (!query.is_empty()).then(|| domain_hash("query", query.as_bytes())),
        query_bytes: query.len(),
        task_blake3: (!task.is_empty()).then(|| domain_hash("task", task.as_bytes())),
        task_bytes: task.len(),
        max_tokens: arguments
            .get("max_tokens")
            .or_else(|| arguments.get("token_budget"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        expected_hash_supplied: arguments
            .get("expected_hash")
            .is_some_and(|value| !value.is_null()),
        known_hash_count: arguments
            .get("known_hashes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        consistency: arguments
            .get("consistency")
            .and_then(Value::as_str)
            .unwrap_or("committed")
            .to_owned(),
    }
}

fn operation_name(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(operation)) => Some(operation.clone()),
        Some(Value::Object(operation)) => operation
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn sanitize_path(raw: &str) -> String {
    let path = Path::new(raw);
    let unsafe_path = path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if unsafe_path {
        return "<redacted>".to_owned();
    }
    raw.replace('\\', "/")
}

fn response_receipt(output: &Value) -> ResponseReceipt {
    let bytes = value_bytes(output);
    let parsed = parse_embedded_json(output.clone());
    let raw = String::from_utf8_lossy(&bytes);
    let error_class = classify_error(&parsed, &raw);
    let mut ranges = BTreeSet::new();
    collect_ranges(&parsed, &mut ranges);
    ResponseReceipt {
        output_blake3: domain_hash("retrieval-output", &bytes),
        output_bytes: bytes.len(),
        error_class,
        status: find_string(&parsed, "status"),
        repository_generation: find_u64(&parsed, "repository_generation"),
        index_stale: find_bool(&parsed, "index_stale"),
        returned_ranges: ranges.into_iter().take(MAX_RESPONSE_RANGES).collect(),
    }
}

fn parse_embedded_json(value: Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str(text.trim()).unwrap_or(Value::String(text)),
        value => value,
    }
}

fn value_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::String(text) => text.as_bytes().to_vec(),
        value => serde_json::to_vec(value).unwrap_or_default(),
    }
}

fn classify_error(value: &Value, raw: &str) -> Option<&'static str> {
    let lowercase = raw.to_ascii_lowercase();
    let explicit_error = find_bool(value, "is_error") == Some(true)
        || find_string(value, "status").is_some_and(|status| status == "error")
        || lowercase.starts_with("error:")
        || lowercase.contains("\"error\":");
    if !explicit_error {
        return None;
    }
    if lowercase.contains("path is not indexed") {
        Some("not_indexed")
    } else if lowercase.contains("request cancelled") {
        Some("cancelled")
    } else if lowercase.contains("retry") {
        Some("retryable")
    } else if lowercase.contains("invalid") {
        Some("invalid_request")
    } else if lowercase.contains("limit") {
        Some("limit_exceeded")
    } else {
        Some("other")
    }
}

fn collect_ranges(value: &Value, ranges: &mut BTreeSet<RangeReceipt>) {
    if ranges.len() >= MAX_RESPONSE_RANGES {
        return;
    }
    match value {
        Value::Object(object) => {
            if let (Some(path), Some(start_line), Some(end_line)) = (
                object.get("path").and_then(Value::as_str),
                object.get("start_line").and_then(Value::as_u64),
                object.get("end_line").and_then(Value::as_u64),
            ) && let (Ok(start_line), Ok(end_line)) =
                (usize::try_from(start_line), usize::try_from(end_line))
            {
                ranges.insert(RangeReceipt {
                    path: sanitize_path(path),
                    start_line,
                    end_line,
                    content_hash: object
                        .get("content_hash")
                        .and_then(Value::as_str)
                        .filter(|hash| hash.len() <= 128)
                        .map(str::to_owned),
                });
            }
            for (key, child) in object {
                if !matches!(
                    key.as_str(),
                    "content" | "excerpt" | "signature" | "task" | "query" | "arguments"
                ) {
                    collect_ranges(child, ranges);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_ranges(child, ranges);
            }
        }
        _ => {}
    }
}

fn find_string(value: &Value, key: &str) -> Option<String> {
    find_value(value, key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn find_u64(value: &Value, key: &str) -> Option<u64> {
    find_value(value, key).and_then(Value::as_u64)
}

fn find_i64(value: &Value, key: &str) -> Option<i64> {
    find_value(value, key).and_then(Value::as_i64)
}

fn find_bool(value: &Value, key: &str) -> Option<bool> {
    find_value(value, key).and_then(Value::as_bool)
}

fn find_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(object) => object
            .get(key)
            .or_else(|| object.values().find_map(|child| find_value(child, key))),
        Value::Array(values) => values.iter().find_map(|child| find_value(child, key)),
        _ => None,
    }
}

fn validation_kinds(command: &str) -> Vec<&'static str> {
    let command = command.to_ascii_lowercase();
    let mut kinds = Vec::new();
    for (needle, kind) in [
        ("pytest", "tests"),
        ("ruff check", "lint"),
        ("ruff format", "format"),
        ("mypy", "typecheck"),
        ("deptry", "dependency_check"),
        ("uv build", "package_build"),
        ("jscpd", "duplication_check"),
        ("git diff --check", "diff_check"),
    ] {
        if command.contains(needle) {
            kinds.push(kind);
        }
    }
    kinds
}

fn summarize(calls: &[RetrievalCall], validations: &[ValidationCall]) -> TrajectorySummary {
    let mut summary = TrajectorySummary {
        retrieval_calls: calls.len(),
        validation_calls: validations.len(),
        ..TrajectorySummary::default()
    };
    let mut unique_paths = BTreeSet::new();
    let mut exact_reads = BTreeSet::new();
    let mut prior_ranges: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();

    for call in calls {
        *summary.calls_by_tool.entry(call.tool.clone()).or_default() += 1;
        if let Some(path) = call.request.path.as_deref() {
            unique_paths.insert(path);
        }
        unique_paths.extend(call.request.paths.iter().map(String::as_str));
        summary.expected_hash_inputs += usize::from(call.request.expected_hash_supplied);
        summary.known_hash_inputs = summary
            .known_hash_inputs
            .saturating_add(call.request.known_hash_count);
        match call.outcome {
            Outcome::Error => summary.failed_calls = summary.failed_calls.saturating_add(1),
            Outcome::NotRecorded => {
                summary.unresolved_outcomes = summary.unresolved_outcomes.saturating_add(1);
            }
            Outcome::Success => {}
        }
        if call.tool != "read" {
            continue;
        }
        let (Some(path), Some(start), Some(end)) = (
            call.request.path.as_deref(),
            call.request.start_line,
            call.request.end_line,
        ) else {
            continue;
        };
        if end < start {
            continue;
        }
        let span = end.saturating_sub(start).saturating_add(1);
        summary.requested_line_spans = summary
            .requested_line_spans
            .saturating_add(u64::try_from(span).unwrap_or(u64::MAX));
        summary.reads_longer_than_100_lines += usize::from(span > 100);
        if !exact_reads.insert((path, start, end)) {
            summary.exact_read_repeats = summary.exact_read_repeats.saturating_add(1);
        }
        let ranges = prior_ranges.entry(path).or_default();
        if ranges.iter().any(|&(prior_start, prior_end)| {
            (prior_start, prior_end) != (start, end) && prior_start <= end && prior_end >= start
        }) {
            summary.overlapping_nonidentical_reads =
                summary.overlapping_nonidentical_reads.saturating_add(1);
        }
        ranges.push((start, end));
    }
    summary.unique_request_paths = unique_paths.len();
    for validation in validations {
        match validation.outcome {
            Outcome::Success => {
                summary.successful_validations = summary.successful_validations.saturating_add(1);
            }
            Outcome::Error => {
                summary.failed_validations = summary.failed_validations.saturating_add(1);
            }
            Outcome::NotRecorded => {}
        }
    }
    summary
}

fn domain_hash(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<(), DynError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_rejects_absolute_and_parent_paths() {
        assert_eq!(sanitize_path("/private/source.rs"), "<redacted>");
        assert_eq!(sanitize_path("../private/source.rs"), "<redacted>");
        assert_eq!(sanitize_path("src\\lib.rs"), "src/lib.rs");
    }

    #[test]
    fn response_receipt_omits_source_but_preserves_range_identity() {
        let output = serde_json::json!({
            "content": "private source",
            "path": "src/lib.rs",
            "start_line": 2,
            "end_line": 4,
            "content_hash": "abc",
            "meta": {"repository_generation": 7}
        });

        let receipt = response_receipt(&output);

        assert_eq!(receipt.error_class, None);
        assert_eq!(receipt.repository_generation, Some(7));
        assert_eq!(
            receipt.returned_ranges,
            vec![RangeReceipt {
                path: "src/lib.rs".into(),
                start_line: 2,
                end_line: 4,
                content_hash: Some("abc".into()),
            }]
        );
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("private source")
        );
    }

    #[test]
    fn response_receipt_classifies_index_error_without_copying_detail() {
        let output =
            Value::String("Error: path is not indexed: private/path.py::Owner.method".to_owned());

        let receipt = response_receipt(&output);

        assert_eq!(receipt.error_class, Some("not_indexed"));
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("private/path")
        );
    }
}
