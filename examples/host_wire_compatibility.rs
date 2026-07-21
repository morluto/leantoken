//! Validate the checked real-host MCP result compatibility matrix.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

type AnyResult<T> = Result<T, Box<dyn Error>>;

const MODES: [&str; 3] = ["dual", "structured", "text"];
const REQUIRED_HOSTS: [&str; 5] = [
    "claude-code",
    "codex-cli",
    "cursor",
    "gemini-cli",
    "opencode",
];

#[derive(Debug, Parser)]
#[command(about = "Validate real-host MCP result compatibility evidence")]
struct Args {
    /// Checked compatibility matrix.
    #[arg(long)]
    matrix: PathBuf,
    /// Repository root used to resolve committed evidence paths.
    #[arg(long, default_value = ".")]
    repository_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
struct Matrix {
    schema_version: u32,
    experiment: String,
    audit_date: String,
    policy: Policy,
    evidence: Vec<Evidence>,
    host_observations: Vec<HostObservation>,
    limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Policy {
    global_default: String,
    unknown_values_are_null: bool,
    eligible_smaller_mode_host_versions: Vec<String>,
    decision: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Evidence {
    id: String,
    kind: String,
    path: PathBuf,
    blake3: String,
}

#[derive(Clone, Debug, Deserialize)]
struct HostObservation {
    host: String,
    version: Option<String>,
    availability: String,
    availability_note: String,
    modes: Vec<ModeObservation>,
}

#[derive(Clone, Debug, Deserialize)]
struct ModeObservation {
    mode: String,
    wire_transport: String,
    model_consumption: String,
    provider_usage: String,
    provider_framing: String,
    complete_wire_tokens: Option<u64>,
    result_text_tokens: Option<u64>,
    structured_content_tokens: Option<u64>,
    duplicated_result_tokens: Option<u64>,
    result_json_bytes: Option<u64>,
    result_text_bytes: Option<u64>,
    structured_content_bytes: Option<u64>,
    emitted_source_tokens: Option<u64>,
    provider_tokens: Option<ProviderTokens>,
    evidence_ids: Vec<String>,
    conclusion: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderTokens {
    #[serde(alias = "total_input_tokens")]
    total_input: Option<u64>,
    #[serde(alias = "uncached_input_tokens")]
    uncached_input: Option<u64>,
    #[serde(alias = "cache_creation_input_tokens")]
    cache_creation_input: Option<u64>,
    #[serde(alias = "cache_read_input_tokens")]
    cache_read_input: Option<u64>,
    #[serde(alias = "output_tokens")]
    output: Option<u64>,
    #[serde(alias = "reasoning_tokens", alias = "reasoning_output_tokens")]
    reasoning: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ValidationSummary {
    schema_version: u32,
    experiment: String,
    audit_date: String,
    global_default: String,
    host_families: usize,
    host_version_observations: usize,
    model_consumption_proofs: usize,
    unavailable_host_families: Vec<String>,
    evidence_artifacts_verified: usize,
    decision: String,
}

#[derive(Debug, Deserialize)]
struct WireAnalysisV2 {
    schema_version: u32,
    host: String,
    host_version: String,
    token_count_exact: bool,
    total_serialized_json_tokens: u64,
    provider_total_input_tokens: Option<u64>,
    provider_usage: ProviderTokens,
    event_categories: BTreeMap<String, EventCost>,
    components: BTreeMap<String, ComponentCost>,
    tool_result_modes: BTreeMap<String, u64>,
    exact_text_structured_duplicates: u64,
    duplicated_result_tokens: u64,
    required_exchange_parts: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize)]
struct EventCost {
    events: u64,
    serialized_json_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ComponentCost {
    occurrences: u64,
    local_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct HostReceipt {
    schema_version: u32,
    host: String,
    host_version: String,
    token_count_exact: bool,
    total_input_tokens: u64,
    provider_usage: ProviderTokens,
    tool_calls: Vec<ReceiptToolCall>,
    mcp_correlation: McpCorrelation,
    privacy: HostReceiptPrivacy,
}

#[derive(Debug, Deserialize)]
struct ReceiptToolCall {
    followed_by_model_response: bool,
    followed_by_provider_usage: bool,
}

#[derive(Debug, Deserialize)]
struct McpCorrelation {
    rollout_tool_calls: u64,
    mcp_tool_calls: u64,
    tool_order_matches: bool,
    protocol_order_valid: bool,
    semantic_output_matches: u64,
    all_semantic_outputs_match: bool,
}

#[derive(Debug, Deserialize)]
struct HostReceiptPrivacy {
    raw_rollout_retained: bool,
    raw_mcp_messages_retained: bool,
    prompts_retained: bool,
    tool_arguments_retained: bool,
    tool_outputs_retained: bool,
    credentials_retained: bool,
    absolute_paths_retained: bool,
    session_and_call_ids_hashed_or_omitted: bool,
}

#[derive(Debug, Deserialize)]
struct StructuredReceipt {
    schema_version: u32,
    experiment_id: String,
    arm: String,
    host: String,
    task_evaluation: TaskEvaluation,
    thread_receipts: Vec<ThreadReceipt>,
    privacy: StructuredReceiptPrivacy,
}

#[derive(Debug, Deserialize)]
struct TaskEvaluation {
    answer_json_valid: bool,
    expected_evidence_count: u64,
    reported_evidence_count: u64,
    matched_path_count: u64,
    matched_exact_evidence_count: u64,
    unexpected_evidence_count: u64,
    task_success: bool,
}

#[derive(Debug, Deserialize)]
struct ThreadReceipt {
    role: String,
    host_version: String,
    provider_usage: ProviderTokens,
    tool_calls: ThreadToolCalls,
}

#[derive(Debug, Deserialize)]
struct ThreadToolCalls {
    mcp: u64,
    failed_mcp: u64,
    mcp_result_json_bytes: u64,
    mcp_text_content_bytes: u64,
    mcp_structured_content_bytes: u64,
    mcp_emitted_source_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct StructuredReceiptPrivacy {
    raw_rollouts_retained: bool,
    prompts_retained: bool,
    tool_arguments_retained: bool,
    tool_outputs_retained: bool,
    credentials_retained: bool,
    absolute_paths_retained: bool,
    session_call_and_agent_ids_hashed_or_omitted: bool,
}

#[derive(Debug, Deserialize)]
struct WireAnalysisV1 {
    schema_version: u32,
    host: String,
    host_version: String,
    token_count_exact: bool,
    event_count: u64,
    total_local_tokens: u64,
    total_provider_input_tokens: Option<u64>,
    categories: BTreeMap<String, LegacyEventCost>,
    tool_result_modes: BTreeMap<String, u64>,
    required_exchange_parts: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize)]
struct LegacyEventCost {
    events: u64,
    local_tokens: u64,
    provider_input_tokens: Option<u64>,
}

fn main() -> AnyResult<()> {
    let args = Args::parse();
    let matrix: Matrix = read_json(&args.matrix)?;
    let summary = validate(&matrix, &args.repository_root)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn validate(matrix: &Matrix, repository_root: &Path) -> AnyResult<ValidationSummary> {
    validate_matrix(matrix)?;
    let evidence = validate_evidence(matrix, repository_root)?;
    validate_codex_01441(matrix, &evidence)?;
    validate_codex_01445(matrix, &evidence)?;

    let host_families = matrix
        .host_observations
        .iter()
        .map(|observation| observation.host.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let model_consumption_proofs = matrix
        .host_observations
        .iter()
        .flat_map(|observation| &observation.modes)
        .filter(|mode| mode.model_consumption == "proven")
        .count();
    let unavailable_host_families = matrix
        .host_observations
        .iter()
        .filter(|observation| observation.availability == "unavailable")
        .map(|observation| observation.host.clone())
        .collect();

    Ok(ValidationSummary {
        schema_version: matrix.schema_version,
        experiment: matrix.experiment.clone(),
        audit_date: matrix.audit_date.clone(),
        global_default: matrix.policy.global_default.clone(),
        host_families,
        host_version_observations: matrix.host_observations.len(),
        model_consumption_proofs,
        unavailable_host_families,
        evidence_artifacts_verified: evidence.len(),
        decision: matrix.policy.decision.clone(),
    })
}

fn validate_matrix(matrix: &Matrix) -> AnyResult<()> {
    if matrix.schema_version != 1 || matrix.experiment != "host-wire-compatibility-v1" {
        return Err(invalid_data("unsupported host compatibility matrix schema"));
    }
    if matrix.audit_date != "2026-07-20" {
        return Err(invalid_data("unexpected host compatibility audit date"));
    }
    if matrix.policy.global_default != "dual"
        || !matrix.policy.unknown_values_are_null
        || !matrix.policy.eligible_smaller_mode_host_versions.is_empty()
        || matrix.policy.decision.trim().is_empty()
    {
        return Err(invalid_data(
            "matrix policy does not preserve the dual default",
        ));
    }
    if matrix.limitations.len() < 5 || matrix.limitations.iter().any(|item| item.trim().is_empty())
    {
        return Err(invalid_data("matrix requires explicit limitations"));
    }

    let hosts = matrix
        .host_observations
        .iter()
        .map(|observation| observation.host.as_str())
        .collect::<BTreeSet<_>>();
    if hosts != REQUIRED_HOSTS.into_iter().collect() {
        return Err(invalid_data(
            "matrix does not cover every required host family",
        ));
    }

    let mut observation_keys = BTreeSet::new();
    for observation in &matrix.host_observations {
        if !observation_keys.insert((observation.host.as_str(), observation.version.as_deref())) {
            return Err(invalid_data("duplicate host/version observation"));
        }
        if observation.availability_note.trim().is_empty() {
            return Err(invalid_data(
                "host observation requires an availability note",
            ));
        }
        let modes = observation
            .modes
            .iter()
            .map(|mode| mode.mode.as_str())
            .collect::<BTreeSet<_>>();
        if modes != MODES.into_iter().collect() || observation.modes.len() != MODES.len() {
            return Err(invalid_data(
                "host observation must contain each result mode once",
            ));
        }
        for mode in &observation.modes {
            validate_mode(mode)?;
        }
        if observation.availability == "unavailable" {
            validate_unavailable(observation)?;
        } else if observation.version.is_none() {
            return Err(invalid_data("captured host observation requires a version"));
        }
    }

    let codex_versions = matrix
        .host_observations
        .iter()
        .filter(|observation| observation.host == "codex-cli")
        .filter_map(|observation| observation.version.as_deref())
        .collect::<BTreeSet<_>>();
    if codex_versions != ["0.144.1", "0.144.5"].into_iter().collect() {
        return Err(invalid_data("matrix requires both frozen Codex versions"));
    }

    let mut evidence_ids = BTreeSet::new();
    for item in &matrix.evidence {
        if !evidence_ids.insert(item.id.as_str()) || item.kind.trim().is_empty() {
            return Err(invalid_data(
                "evidence identifiers must be unique and typed",
            ));
        }
        validate_relative_evidence_path(&item.path)?;
        if item.blake3.len() != 64 || !item.blake3.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_data(
                "evidence BLAKE3 must be 64 hexadecimal characters",
            ));
        }
    }
    for mode in matrix
        .host_observations
        .iter()
        .flat_map(|observation| &observation.modes)
    {
        if mode
            .evidence_ids
            .iter()
            .any(|id| !evidence_ids.contains(id.as_str()))
        {
            return Err(invalid_data("mode references unknown evidence"));
        }
    }
    Ok(())
}

fn validate_mode(mode: &ModeObservation) -> AnyResult<()> {
    if !["complete", "tool_results_only", "unknown"].contains(&mode.wire_transport.as_str())
        || !["proven", "unknown"].contains(&mode.model_consumption.as_str())
        || !["partial", "unknown"].contains(&mode.provider_usage.as_str())
        || mode.provider_framing != "unknown"
        || mode.conclusion.trim().is_empty()
    {
        return Err(invalid_data("mode contains an unsupported evidence state"));
    }
    match (mode.provider_usage.as_str(), &mode.provider_tokens) {
        ("partial", Some(tokens))
            if tokens.total_input.is_some()
                && tokens.uncached_input.is_some()
                && tokens.cache_creation_input.is_none()
                && tokens.cache_read_input.is_some()
                && tokens.output.is_some()
                && tokens.reasoning.is_some() => {}
        ("unknown", None) => {}
        _ => {
            return Err(invalid_data(
                "provider token values do not match their evidence state",
            ));
        }
    }
    if mode.model_consumption == "proven" && mode.evidence_ids.is_empty() {
        return Err(invalid_data("model-consumption proof requires evidence"));
    }
    Ok(())
}

fn validate_unavailable(observation: &HostObservation) -> AnyResult<()> {
    if observation.version.is_some() {
        return Err(invalid_data("unavailable host version must remain null"));
    }
    for mode in &observation.modes {
        let measurements_absent = mode.complete_wire_tokens.is_none()
            && mode.result_text_tokens.is_none()
            && mode.structured_content_tokens.is_none()
            && mode.duplicated_result_tokens.is_none()
            && mode.result_json_bytes.is_none()
            && mode.result_text_bytes.is_none()
            && mode.structured_content_bytes.is_none()
            && mode.emitted_source_tokens.is_none()
            && mode.provider_tokens.is_none();
        if mode.wire_transport != "unknown"
            || mode.model_consumption != "unknown"
            || mode.provider_usage != "unknown"
            || !mode.evidence_ids.is_empty()
            || !measurements_absent
        {
            return Err(invalid_data(
                "unavailable host measurements must remain unknown and null",
            ));
        }
    }
    Ok(())
}

fn validate_evidence(
    matrix: &Matrix,
    repository_root: &Path,
) -> AnyResult<BTreeMap<String, Vec<u8>>> {
    let mut loaded = BTreeMap::new();
    for evidence in &matrix.evidence {
        let bytes = canonical_json_artifact(fs::read(repository_root.join(&evidence.path))?)?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != evidence.blake3 {
            return Err(invalid_data(&format!(
                "evidence hash mismatch for {}",
                evidence.id
            )));
        }
        loaded.insert(evidence.id.clone(), bytes);
    }
    Ok(loaded)
}

fn canonical_json_artifact(bytes: Vec<u8>) -> AnyResult<Vec<u8>> {
    let text = String::from_utf8(bytes)?;
    let normalized = text.replace("\r\n", "\n");
    if normalized.contains('\r') {
        return Err(invalid_data(
            "evidence JSON contains a lone carriage return",
        ));
    }
    Ok(normalized.into_bytes())
}

fn validate_codex_01441(matrix: &Matrix, evidence: &BTreeMap<String, Vec<u8>>) -> AnyResult<()> {
    let wire: WireAnalysisV2 = evidence_json(evidence, "codex-0.144.1-dual-wire")?;
    let receipt: HostReceipt = evidence_json(evidence, "codex-0.144.1-host-receipt")?;
    let structured: StructuredReceipt = evidence_json(evidence, "codex-0.144.1-structured-owner")?;

    if wire.schema_version != 2
        || wire.host != "codex-cli"
        || wire.host_version != "0.144.1"
        || !wire.token_count_exact
        || wire.total_serialized_json_tokens != 4483
        || wire.provider_total_input_tokens.is_some()
        || provider_has_value(&wire.provider_usage)
        || !required_exchange_complete(&wire.required_exchange_parts)
        || wire.tool_result_modes.get("dual") != Some(&3)
        || wire.exact_text_structured_duplicates != 3
        || wire.duplicated_result_tokens != 776
        || event_cost(&wire.event_categories, "initialize_request")? != (1, 67)
        || event_cost(&wire.event_categories, "initialize_response")? != (1, 118)
        || event_cost(&wire.event_categories, "initialized_notification")? != (1, 14)
        || event_cost(&wire.event_categories, "tool_call_request")? != (3, 949)
        || event_cost(&wire.event_categories, "tool_call_response")? != (3, 1694)
        || event_cost(&wire.event_categories, "tools_list_request")? != (1, 27)
        || event_cost(&wire.event_categories, "tools_list_response")? != (1, 1614)
        || component_cost(&wire.components, "catalog")? != (1, 1595)
        || component_cost(&wire.components, "call_arguments")? != (3, 69)
        || component_cost(&wire.components, "result_text")? != (3, 854)
        || component_cost(&wire.components, "structured_content")? != (3, 776)
    {
        return Err(invalid_data("Codex 0.144.1 dual wire evidence changed"));
    }

    let privacy_absent = !receipt.privacy.raw_rollout_retained
        && !receipt.privacy.raw_mcp_messages_retained
        && !receipt.privacy.prompts_retained
        && !receipt.privacy.tool_arguments_retained
        && !receipt.privacy.tool_outputs_retained
        && !receipt.privacy.credentials_retained
        && !receipt.privacy.absolute_paths_retained
        && receipt.privacy.session_and_call_ids_hashed_or_omitted;
    if receipt.schema_version != 1
        || receipt.host != "codex-cli"
        || receipt.host_version != "0.144.1"
        || !receipt.token_count_exact
        || receipt.total_input_tokens != 70904
        || receipt.tool_calls.len() != 3
        || receipt
            .tool_calls
            .iter()
            .any(|call| !call.followed_by_model_response || !call.followed_by_provider_usage)
        || receipt.mcp_correlation.rollout_tool_calls != 3
        || receipt.mcp_correlation.mcp_tool_calls != 3
        || !receipt.mcp_correlation.tool_order_matches
        || !receipt.mcp_correlation.protocol_order_valid
        || receipt.mcp_correlation.semantic_output_matches != 3
        || !receipt.mcp_correlation.all_semantic_outputs_match
        || !privacy_absent
    {
        return Err(invalid_data("Codex 0.144.1 host receipt evidence changed"));
    }
    require_provider_tokens(
        &receipt.provider_usage,
        [None, Some(7672), None, Some(63232), Some(282), Some(87)],
    )?;

    let structured_privacy_absent = !structured.privacy.raw_rollouts_retained
        && !structured.privacy.prompts_retained
        && !structured.privacy.tool_arguments_retained
        && !structured.privacy.tool_outputs_retained
        && !structured.privacy.credentials_retained
        && !structured.privacy.absolute_paths_retained
        && structured
            .privacy
            .session_call_and_agent_ids_hashed_or_omitted;
    let child = structured
        .thread_receipts
        .iter()
        .find(|thread| thread.role == "subagent")
        .ok_or_else(|| invalid_data("structured receipt has no child thread"))?;
    let task = &structured.task_evaluation;
    if structured.schema_version != 1
        || structured.experiment_id != "multi-agent-context-pilot"
        || structured.arm != "thin-leantoken-structured-owner"
        || structured.host != "codex-cli"
        || structured.thread_receipts.len() != 2
        || child.host_version != "0.144.1"
        || !task.answer_json_valid
        || task.expected_evidence_count != 4
        || task.reported_evidence_count != 4
        || task.matched_path_count != 4
        || task.matched_exact_evidence_count != 4
        || task.unexpected_evidence_count != 0
        || !task.task_success
        || child.tool_calls.mcp != 6
        || child.tool_calls.failed_mcp != 0
        || child.tool_calls.mcp_result_json_bytes != 38127
        || child.tool_calls.mcp_text_content_bytes != 0
        || child.tool_calls.mcp_structured_content_bytes != 37821
        || child.tool_calls.mcp_emitted_source_tokens != 5265
        || !structured_privacy_absent
    {
        return Err(invalid_data(
            "Codex 0.144.1 structured model-consumption evidence changed",
        ));
    }
    require_provider_tokens(
        &child.provider_usage,
        [
            Some(83923),
            Some(17619),
            None,
            Some(66304),
            Some(658),
            Some(217),
        ],
    )?;

    let observation = host_observation(matrix, "codex-cli", "0.144.1")?;
    let dual = mode_observation(observation, "dual")?;
    if dual.complete_wire_tokens != Some(wire.total_serialized_json_tokens)
        || dual.result_text_tokens != Some(component_cost(&wire.components, "result_text")?.1)
        || dual.structured_content_tokens
            != Some(component_cost(&wire.components, "structured_content")?.1)
        || dual.duplicated_result_tokens != Some(wire.duplicated_result_tokens)
        || !provider_tokens_equal_with_total(
            dual.provider_tokens.as_ref(),
            receipt.total_input_tokens,
            &receipt.provider_usage,
        )
    {
        return Err(invalid_data("Codex 0.144.1 dual matrix values drifted"));
    }
    let structured_mode = mode_observation(observation, "structured")?;
    if structured_mode.result_json_bytes != Some(child.tool_calls.mcp_result_json_bytes)
        || structured_mode.result_text_bytes != Some(child.tool_calls.mcp_text_content_bytes)
        || structured_mode.structured_content_bytes
            != Some(child.tool_calls.mcp_structured_content_bytes)
        || structured_mode.emitted_source_tokens != Some(child.tool_calls.mcp_emitted_source_tokens)
        || !provider_tokens_equal(
            structured_mode.provider_tokens.as_ref(),
            &child.provider_usage,
        )
    {
        return Err(invalid_data(
            "Codex 0.144.1 structured matrix values drifted",
        ));
    }
    Ok(())
}

fn validate_codex_01445(matrix: &Matrix, evidence: &BTreeMap<String, Vec<u8>>) -> AnyResult<()> {
    let wire: WireAnalysisV1 = evidence_json(evidence, "codex-0.144.5-dual-wire")?;
    if wire.schema_version != 1
        || wire.host != "codex-cli"
        || wire.host_version != "0.144.5"
        || !wire.token_count_exact
        || wire.event_count != 9
        || wire.total_local_tokens != 2896
        || wire.total_provider_input_tokens.is_some()
        || !required_exchange_complete(&wire.required_exchange_parts)
        || wire.tool_result_modes.get("dual") != Some(&2)
        || legacy_event_cost(&wire.categories, "initialize_request")? != (1, 67)
        || legacy_event_cost(&wire.categories, "initialize_response")? != (1, 97)
        || legacy_event_cost(&wire.categories, "initialized_notification")? != (1, 14)
        || legacy_event_cost(&wire.categories, "tool_call_request")? != (2, 530)
        || legacy_event_cost(&wire.categories, "tool_call_response")? != (2, 607)
        || legacy_event_cost(&wire.categories, "tools_list_request")? != (1, 27)
        || legacy_event_cost(&wire.categories, "tools_list_response")? != (1, 1554)
        || wire
            .categories
            .values()
            .any(|category| category.provider_input_tokens.is_some())
    {
        return Err(invalid_data("Codex 0.144.5 wire evidence changed"));
    }
    let dual = mode_observation(host_observation(matrix, "codex-cli", "0.144.5")?, "dual")?;
    if dual.complete_wire_tokens != Some(wire.total_local_tokens) {
        return Err(invalid_data("Codex 0.144.5 matrix values drifted"));
    }
    Ok(())
}

fn event_cost(categories: &BTreeMap<String, EventCost>, name: &str) -> AnyResult<(u64, u64)> {
    categories
        .get(name)
        .map(|cost| (cost.events, cost.serialized_json_tokens))
        .ok_or_else(|| invalid_data(&format!("missing wire category {name}")))
}

fn component_cost(
    components: &BTreeMap<String, ComponentCost>,
    name: &str,
) -> AnyResult<(u64, u64)> {
    components
        .get(name)
        .map(|cost| (cost.occurrences, cost.local_tokens))
        .ok_or_else(|| invalid_data(&format!("missing wire component {name}")))
}

fn legacy_event_cost(
    categories: &BTreeMap<String, LegacyEventCost>,
    name: &str,
) -> AnyResult<(u64, u64)> {
    categories
        .get(name)
        .map(|cost| (cost.events, cost.local_tokens))
        .ok_or_else(|| invalid_data(&format!("missing legacy wire category {name}")))
}

fn required_exchange_complete(parts: &BTreeMap<String, bool>) -> bool {
    [
        "initialize_request",
        "initialize_response",
        "initialized_notification",
        "tool_result",
        "tools_call",
        "tools_list",
    ]
    .into_iter()
    .all(|part| parts.get(part) == Some(&true))
}

fn provider_has_value(tokens: &ProviderTokens) -> bool {
    [
        tokens.total_input,
        tokens.uncached_input,
        tokens.cache_creation_input,
        tokens.cache_read_input,
        tokens.output,
        tokens.reasoning,
    ]
    .into_iter()
    .any(|value| value.is_some())
}

fn require_provider_tokens(actual: &ProviderTokens, expected: [Option<u64>; 6]) -> AnyResult<()> {
    let values = [
        actual.total_input,
        actual.uncached_input,
        actual.cache_creation_input,
        actual.cache_read_input,
        actual.output,
        actual.reasoning,
    ];
    if values != expected {
        return Err(invalid_data("provider token evidence changed"));
    }
    Ok(())
}

fn provider_tokens_equal(left: Option<&ProviderTokens>, right: &ProviderTokens) -> bool {
    left.is_some_and(|left| {
        left.total_input == right.total_input
            && left.uncached_input == right.uncached_input
            && left.cache_creation_input == right.cache_creation_input
            && left.cache_read_input == right.cache_read_input
            && left.output == right.output
            && left.reasoning == right.reasoning
    })
}

fn provider_tokens_equal_with_total(
    left: Option<&ProviderTokens>,
    total_input: u64,
    right: &ProviderTokens,
) -> bool {
    left.is_some_and(|left| {
        left.total_input == Some(total_input)
            && left.uncached_input == right.uncached_input
            && left.cache_creation_input == right.cache_creation_input
            && left.cache_read_input == right.cache_read_input
            && left.output == right.output
            && left.reasoning == right.reasoning
    })
}

fn host_observation<'a>(
    matrix: &'a Matrix,
    host: &str,
    version: &str,
) -> AnyResult<&'a HostObservation> {
    matrix
        .host_observations
        .iter()
        .find(|observation| {
            observation.host == host && observation.version.as_deref() == Some(version)
        })
        .ok_or_else(|| invalid_data(&format!("missing host observation {host} {version}")))
}

fn mode_observation<'a>(
    observation: &'a HostObservation,
    mode: &str,
) -> AnyResult<&'a ModeObservation> {
    observation
        .modes
        .iter()
        .find(|observation| observation.mode == mode)
        .ok_or_else(|| invalid_data(&format!("missing mode observation {mode}")))
}

fn evidence_json<T: for<'de> Deserialize<'de>>(
    evidence: &BTreeMap<String, Vec<u8>>,
    id: &str,
) -> AnyResult<T> {
    let bytes = evidence
        .get(id)
        .ok_or_else(|| invalid_data(&format!("missing evidence {id}")))?;
    Ok(serde_json::from_slice(bytes)?)
}

fn validate_relative_evidence_path(path: &Path) -> AnyResult<()> {
    let mut components = path.components();
    if components.next() != Some(Component::Normal("benchmarks".as_ref()))
        || components.next() != Some(Component::Normal("reports".as_ref()))
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data(
            "evidence path must be a safe benchmarks/reports relative path",
        ));
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> AnyResult<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn invalid_data(message: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_matrix() -> (PathBuf, Matrix) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let matrix = read_json(&root.join("benchmarks/reports/host-wire-compatibility-v1.json"))
            .expect("checked matrix");
        (root, matrix)
    }

    #[test]
    fn checked_matrix_matches_all_committed_evidence() {
        let (root, matrix) = checked_matrix();
        let summary = validate(&matrix, &root).expect("valid matrix");

        assert_eq!(summary.host_families, 5);
        assert_eq!(summary.host_version_observations, 6);
        assert_eq!(summary.model_consumption_proofs, 2);
        assert_eq!(summary.evidence_artifacts_verified, 4);
        assert_eq!(summary.unavailable_host_families.len(), 4);
    }

    #[test]
    fn unavailable_host_cannot_report_zero_as_a_measurement() {
        let (_, mut matrix) = checked_matrix();
        let unavailable = matrix
            .host_observations
            .iter_mut()
            .find(|observation| observation.availability == "unavailable")
            .expect("unavailable host");
        unavailable.modes[0].complete_wire_tokens = Some(0);

        let error = validate_matrix(&matrix).expect_err("imputed zero must fail");
        assert!(
            error
                .to_string()
                .contains("measurements must remain unknown and null")
        );
    }

    #[test]
    fn every_host_requires_all_three_modes() {
        let (_, mut matrix) = checked_matrix();
        matrix.host_observations[0].modes.pop();

        let error = validate_matrix(&matrix).expect_err("missing mode must fail");
        assert!(error.to_string().contains("each result mode once"));
    }

    #[test]
    fn artifact_identity_normalizes_windows_checkout_line_endings() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bytes = fs::read(root.join("benchmarks/reports/wire-trace-codex-cli-0.144.1.json"))
            .expect("wire evidence");
        let canonical = canonical_json_artifact(bytes).expect("canonical evidence");
        let crlf = String::from_utf8(canonical.clone())
            .expect("UTF-8 evidence")
            .replace('\n', "\r\n")
            .into_bytes();
        let normalized = canonical_json_artifact(crlf).expect("normalized evidence");

        assert_eq!(normalized, canonical);
        assert_eq!(blake3::hash(&normalized), blake3::hash(&canonical));
    }
}
