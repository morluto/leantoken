//! Measure frozen MCP response representation ablations and their safety gates.

use std::collections::{BTreeSet, HashSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use leantoken::Config;
use leantoken::mcp::{LeanTokenMcp, McpResultMode, tool_catalog_json, tool_result};
use leantoken::model::{
    ContextRequest, ContextResponse, FileOperation, FilesRequest, OutlineRequest,
};
use leantoken::services::Services;
use leantoken::tokens::Tokenizer;
use rmcp::{ServerHandler, model::ProtocolVersion};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(about = "Run the frozen complete MCP response ablation")]
struct Args {
    /// Frozen experiment manifest.
    #[arg(long, default_value = "benchmarks/mcp_response_ablation.json")]
    manifest: PathBuf,
    /// Repository root used to resolve fixture and evidence paths.
    #[arg(long, default_value = ".")]
    repository_root: PathBuf,
    /// Versioned JSON report destination.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    experiment: String,
    fixture: FixtureManifest,
    host_compatibility_evidence: HostEvidence,
    candidates: Vec<String>,
    acceptance: Acceptance,
}

#[derive(Debug, Deserialize)]
struct FixtureManifest {
    root: PathBuf,
    tree_blake3: String,
    task: String,
    token_budget: usize,
    tokenizer: String,
    canonicalize_line_endings: bool,
}

#[derive(Debug, Deserialize)]
struct HostEvidence {
    path: PathBuf,
    blake3: String,
    global_default: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Acceptance {
    require_exact_local_token_count: bool,
    require_negative_complete_wire_token_delta: bool,
    require_round_trip: bool,
    require_freshness_semantics: bool,
    require_range_identity: bool,
    require_known_hash_deduplication: bool,
    maximum_additional_exact_resend_source_tokens: usize,
    maximum_additional_overlapping_source_tokens: usize,
    non_dual_mode_requires_exact_host_version_evidence: bool,
    provider_values_must_remain_null_when_unavailable: bool,
}

#[derive(Debug, Deserialize)]
struct HostMatrix {
    policy: HostPolicy,
    host_observations: Vec<HostObservation>,
}

#[derive(Debug, Deserialize)]
struct HostPolicy {
    global_default: String,
}

#[derive(Debug, Deserialize)]
struct HostObservation {
    host: String,
    version: Option<String>,
    modes: Vec<HostMode>,
}

#[derive(Debug, Deserialize)]
struct HostMode {
    mode: String,
    model_consumption: String,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    experiment: String,
    report_date: &'static str,
    fixture: FixtureReport,
    acceptance: Acceptance,
    baseline: Baseline,
    follow_up: FollowUp,
    candidates: Vec<CandidateReport>,
    decision: Decision,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct FixtureReport {
    root: String,
    tree_blake3: String,
    task: String,
    token_budget: usize,
    tokenizer: String,
    token_count_exact: bool,
    host_compatibility_path: String,
    host_compatibility_blake3: String,
}

#[derive(Debug, Serialize)]
struct Baseline {
    source_tokens: usize,
    response_json_tokens: usize,
    catalog_tokens: usize,
    dual_result_tokens: usize,
    text_result_tokens: usize,
    structured_result_tokens: usize,
    complete_dual_wire_tokens: usize,
    fragment_count: usize,
    receipt_hash_count: usize,
    omitted_detail_count: usize,
    warning_count: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct FollowUp {
    exact_resend_source_tokens: usize,
    overlapping_source_tokens: usize,
    known_hash_omission_count: usize,
}

#[derive(Debug, Serialize)]
struct CandidateReport {
    id: String,
    change: &'static str,
    measurements: Vec<Comparison>,
    round_trip: bool,
    freshness_semantics: bool,
    range_identity: bool,
    known_hash_deduplication: bool,
    model_behavior_evidence: &'static str,
    host_compatibility: &'static str,
    decision: &'static str,
    rationale: &'static str,
}

#[derive(Debug, Serialize)]
struct Comparison {
    surface: &'static str,
    mode: &'static str,
    baseline: Measurement,
    candidate: Measurement,
    response_json_token_delta: i64,
    serialized_result_token_delta: i64,
    complete_wire_token_delta: i64,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Measurement {
    source_tokens: usize,
    response_json_tokens: usize,
    serialized_result_tokens: usize,
    complete_wire_tokens: usize,
    provider_native_input_tokens: Option<usize>,
    exact_resend_source_tokens: usize,
    overlapping_source_tokens: usize,
}

#[derive(Debug, Serialize)]
struct Decision {
    accepted_new_runtime_change: Vec<&'static str>,
    retained_existing_compactions: Vec<&'static str>,
    host_scoped_opt_in: Vec<&'static str>,
    rejected_candidates: Vec<&'static str>,
    global_result_mode: &'static str,
    provider_input_conclusion: &'static str,
}

#[derive(Clone)]
struct WireContext<'a> {
    tokenizer: Tokenizer,
    initialize_request: Value,
    initialize_response: Value,
    initialized_notification: Value,
    tools_list_request: Value,
    catalog: &'a Value,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    let manifest: Manifest = read_json(&args.manifest)?;
    let report = generate(&manifest, &args.repository_root).await?;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    Ok(())
}

async fn generate(manifest: &Manifest, repository_root: &Path) -> AnyResult<Report> {
    validate_manifest(manifest)?;
    let fixture_source = repository_root.join(&manifest.fixture.root);
    let fixture_files = canonical_fixture_files(&fixture_source)?;
    let fixture_hash = fixture_manifest_hash(&fixture_files);
    if fixture_hash != manifest.fixture.tree_blake3 {
        return Err(invalid_data("fixture tree commitment mismatch"));
    }
    let host_matrix_bytes = canonical_json(fs::read(
        repository_root.join(&manifest.host_compatibility_evidence.path),
    )?)?;
    let host_hash = blake3::hash(&host_matrix_bytes).to_hex().to_string();
    if host_hash != manifest.host_compatibility_evidence.blake3 {
        return Err(invalid_data("host compatibility commitment mismatch"));
    }
    let host_matrix: HostMatrix = serde_json::from_slice(&host_matrix_bytes)?;
    let codex_structured = validate_host_matrix(manifest, &host_matrix)?;

    let temp = tempfile::tempdir()?;
    let fixture_root = temp.path().join("repo");
    materialize_fixture(&fixture_root, &fixture_files)?;
    let config = Config::discover(&fixture_root, Some(temp.path().join("index.sqlite")))?;
    if config.tokenizer.name() != manifest.fixture.tokenizer || !config.tokenizer.is_exact() {
        return Err(invalid_data(
            "manifest tokenizer is not the exact runtime tokenizer",
        ));
    }
    let tokenizer = config.tokenizer;
    let services = Arc::new(Services::open(config)?);
    services.index(true).await?;

    let request = ContextRequest {
        task: manifest.fixture.task.clone(),
        token_budget: manifest.fixture.token_budget,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    };
    let response = services.context(request.clone()).await?;
    validate_context_semantics(&response)?;
    let follow_up_response = services
        .context(ContextRequest {
            known_hashes: response.receipt.fragment_hashes.clone(),
            prior_repository_generation: Some(response.meta.repository_generation),
            ..request.clone()
        })
        .await?;
    let follow_up = follow_up_metrics(&response, &follow_up_response);

    let outline_empty = services
        .outline(OutlineRequest {
            paths: vec!["README.md".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(20),
            max_tokens: Some(8_000),
        })
        .await?;
    let outline_rich = services
        .outline(OutlineRequest {
            paths: vec!["src/rust/math.rs".into()],
            symbol_name: None,
            symbol_kind: None,
            max_results: Some(20),
            max_tokens: Some(8_000),
        })
        .await?;
    let files = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(2),
        })
        .await?;

    let catalog: Value = serde_json::from_str(&tool_catalog_json())?;
    let server = LeanTokenMcp::new(Arc::clone(&services));
    let mut server_info = server.get_info();
    server_info.protocol_version = ProtocolVersion::LATEST;
    let wire = WireContext {
        tokenizer,
        initialize_request: json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": ProtocolVersion::LATEST.as_str(),
                "capabilities": {},
                "clientInfo": { "name": "benchmark-client", "version": "1" }
            }
        }),
        initialize_response: json!({"jsonrpc": "2.0", "id": 0, "result": server_info}),
        initialized_notification: json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }),
        tools_list_request: json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}
        }),
        catalog: &catalog,
    };

    let context_call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "leantoken_context",
            "arguments": {
                "task": manifest.fixture.task,
                "token_budget": manifest.fixture.token_budget,
                "focus_paths": [],
                "focus_symbols": [],
                "exclude_paths": [],
                "known_hashes": [],
                "prior_repository_generation": null
            }
        }
    });
    let outline_empty_call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "leantoken_outline", "arguments": {"paths": ["README.md"]}}
    });
    let outline_rich_call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "leantoken_outline", "arguments": {"paths": ["src/rust/math.rs"]}}
    });
    let files_call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "leantoken_files", "arguments": {
            "operation": {"kind": "tree", "depth": 2}, "max_results": 20
        }}
    });

    let compact_context = serde_json::to_value(&response)?;
    let pre_change_context = with_task_fingerprint(compact_context.clone(), &response)?;
    let baseline_dual = measure(
        &wire,
        &context_call,
        &pre_change_context,
        McpResultMode::Dual,
        response.meta.emitted_tokens,
        follow_up,
    )?;
    let baseline = Baseline {
        source_tokens: response.meta.emitted_tokens,
        response_json_tokens: baseline_dual.response_json_tokens,
        catalog_tokens: tokenizer.count(&catalog.to_string()),
        dual_result_tokens: baseline_dual.serialized_result_tokens,
        text_result_tokens: measure(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Text,
            response.meta.emitted_tokens,
            follow_up,
        )?
        .serialized_result_tokens,
        structured_result_tokens: measure(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Structured,
            response.meta.emitted_tokens,
            follow_up,
        )?
        .serialized_result_tokens,
        complete_dual_wire_tokens: baseline_dual.complete_wire_tokens,
        fragment_count: response.fragments.len(),
        receipt_hash_count: response.receipt.fragment_hashes.len(),
        omitted_detail_count: response.omitted.len(),
        warning_count: response.warnings.len(),
    };

    let mut candidates = Vec::new();
    candidates.push(candidate(
        "structured_mode",
        "send the unchanged response as structuredContent only",
        vec![compare(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            &pre_change_context,
            McpResultMode::Structured,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        true,
        true,
        true,
        true,
        "one frozen Codex task",
        if codex_structured {
            "Codex CLI 0.144.1 only"
        } else {
            "unproven"
        },
        "host_scoped_opt_in",
        "Codex CLI 0.144.1 consumed structured-only results in one frozen task; the global matrix remains incomplete.",
    ));
    candidates.push(candidate(
        "text_mode",
        "send the unchanged response as text content only",
        vec![compare(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            &pre_change_context,
            McpResultMode::Text,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        true,
        true,
        true,
        true,
        "none",
        "no real-host text-only model-consumption proof",
        "rejected",
        "Local serialization savings cannot substitute for host compatibility evidence.",
    ));
    candidates.push(candidate(
        "omit_task_fingerprint",
        "omit the internal task hash from the serialized receipt",
        vec![compare(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            &compact_context,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        serde_json::from_value::<ContextResponse>(compact_context.clone()).is_ok(),
        true,
        true,
        true,
        "not required: source and retrieval are unchanged",
        "representation-neutral",
        "accepted",
        "The request already carries the task, no follow-up accepts the fingerprint, and aligned fragment hashes retain deduplication identity.",
    ));

    let expanded_receipt = expand_receipt_identities(&pre_change_context, &response)?;
    candidates.push(candidate(
        "aligned_receipt_hash_table",
        "retain one hash array aligned with fragments instead of repeated identity objects",
        vec![compare(
            &wire,
            &context_call,
            &expanded_receipt,
            McpResultMode::Dual,
            &pre_change_context,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        serde_json::from_value::<ContextResponse>(pre_change_context.clone()).is_ok(),
        true,
        true,
        true,
        "not required: source and retrieval are unchanged",
        "representation-neutral",
        "retained_existing",
        "The checked response keeps every path/range in fragments and every aligned content hash in the receipt.",
    ));

    let expanded_fragments = expand_fragment_metadata(&pre_change_context, &response)?;
    candidates.push(candidate(
        "compact_fragment_metadata",
        "omit repeated hash, score, and token-count diagnostics from serialized fragments",
        vec![compare(
            &wire,
            &context_call,
            &expanded_fragments,
            McpResultMode::Dual,
            &pre_change_context,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        serde_json::from_value::<ContextResponse>(pre_change_context.clone()).is_ok(),
        true,
        true,
        true,
        "not required: source and retrieval are unchanged",
        "representation-neutral",
        "retained_existing",
        "Receipt hashes, range fields, selection reasons, and aggregate emitted tokens preserve the required semantics.",
    ));

    let expanded_defaults = expand_context_defaults(&pre_change_context, &response)?;
    let outline_empty_value = serde_json::to_value(&outline_empty)?;
    let outline_expanded = expand_outline_defaults(outline_empty_value.clone())?;
    candidates.push(candidate(
        "omit_empty_and_default_fields",
        "omit default source representation, null cursor, and empty outline collections",
        vec![
            compare(
                &wire,
                &context_call,
                &expanded_defaults,
                McpResultMode::Dual,
                &pre_change_context,
                McpResultMode::Dual,
                response.meta.emitted_tokens,
                follow_up,
                "context_response",
            )?,
            compare(
                &wire,
                &outline_empty_call,
                &outline_expanded,
                McpResultMode::Dual,
                &outline_empty_value,
                McpResultMode::Dual,
                outline_empty.meta.emitted_tokens,
                FollowUp::default(),
                "empty_outline_response",
            )?,
        ],
        serde_json::from_value::<ContextResponse>(pre_change_context.clone()).is_ok(),
        true,
        true,
        true,
        "not required: source and retrieval are unchanged",
        "representation-neutral",
        "retained_existing",
        "Serde defaults round-trip each omitted value without hiding freshness or token-count exactness.",
    ));

    let short_reasons = encode_short_reasons(pre_change_context.clone())?;
    candidates.push(candidate(
        "short_reason_codes",
        "replace readable selection reasons with one-letter codes",
        vec![compare(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            &short_reasons,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        serde_json::from_value::<ContextResponse>(short_reasons).is_ok(),
        true,
        true,
        true,
        "none",
        "representation-neutral but model interpretation untested",
        "rejected",
        "The small local delta does not justify making grounded selection reasons opaque without controlled model evidence.",
    ));

    let no_omission_details = remove_field(pre_change_context.clone(), "omitted")?;
    candidates.push(candidate(
        "omit_omission_details",
        "remove bounded paths and ranges for omitted candidates",
        vec![compare(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            &no_omission_details,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        serde_json::from_value::<ContextResponse>(no_omission_details).is_ok(),
        true,
        true,
        false,
        "not applicable",
        "representation-neutral",
        "rejected",
        "The token saving removes explicit known-content and budget omission accounting required to diagnose resends.",
    ));

    let no_default_meta = omit_default_meta(pre_change_context.clone())?;
    candidates.push(candidate(
        "omit_default_meta",
        "omit current freshness and exact-token flags",
        vec![compare(
            &wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            &no_default_meta,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "context_response",
        )?],
        serde_json::from_value::<ContextResponse>(no_default_meta).is_ok(),
        false,
        true,
        true,
        "not applicable",
        "representation-neutral",
        "rejected",
        "Freshness and tokenizer exactness are explicit correctness boundaries, not optional diagnostics.",
    ));

    let outline_rich_value = serde_json::to_value(&outline_rich)?;
    let outline_tuples = compact_outline_tuples(outline_rich_value.clone())?;
    candidates.push(candidate(
        "compact_outline_tuples",
        "replace named outline objects with positional arrays",
        vec![compare(
            &wire,
            &outline_rich_call,
            &outline_rich_value,
            McpResultMode::Dual,
            &outline_tuples,
            McpResultMode::Dual,
            outline_rich.meta.emitted_tokens,
            FollowUp::default(),
            "outline_response",
        )?],
        false,
        true,
        false,
        true,
        "none",
        "representation-neutral",
        "rejected",
        "Positional encoding breaks the current typed response and makes line and byte range fields ambiguous to generic clients.",
    ));

    let files_value = serde_json::to_value(&files)?;
    let tree_paths = compact_tree_paths(files_value.clone())?;
    candidates.push(candidate(
        "compact_tree_paths",
        "replace tree entry objects with path strings",
        vec![compare(
            &wire,
            &files_call,
            &files_value,
            McpResultMode::Dual,
            &tree_paths,
            McpResultMode::Dual,
            files.meta.emitted_tokens,
            FollowUp::default(),
            "files_tree_response",
        )?],
        false,
        true,
        false,
        true,
        "none",
        "representation-neutral",
        "rejected",
        "Path strings lose file/directory kind and available language and size metadata.",
    ));

    let no_examples_catalog = remove_tool_examples(catalog.clone())?;
    let candidate_wire = WireContext {
        catalog: &no_examples_catalog,
        ..wire.clone()
    };
    candidates.push(candidate(
        "remove_tool_examples",
        "remove examples from every tool description",
        vec![compare_wire_contexts(
            &wire,
            &candidate_wire,
            &context_call,
            &pre_change_context,
            McpResultMode::Dual,
            response.meta.emitted_tokens,
            follow_up,
            "tool_catalog",
        )?],
        true,
        true,
        true,
        true,
        "none",
        "catalog behavior requires a model call-quality evaluation",
        "rejected",
        "Catalog savings are local-only; removing examples without measuring malformed or extra calls could increase end-to-end cost.",
    ));

    validate_candidate_coverage(manifest, &candidates)?;
    validate_accepted_candidate(manifest, &candidates)?;

    Ok(Report {
        schema_version: 1,
        experiment: manifest.experiment.clone(),
        report_date: "2026-07-21",
        fixture: FixtureReport {
            root: slash_path(&manifest.fixture.root),
            tree_blake3: fixture_hash,
            task: manifest.fixture.task.clone(),
            token_budget: manifest.fixture.token_budget,
            tokenizer: tokenizer.name().to_owned(),
            token_count_exact: tokenizer.is_exact(),
            host_compatibility_path: slash_path(&manifest.host_compatibility_evidence.path),
            host_compatibility_blake3: host_hash,
        },
        acceptance: Acceptance {
            require_exact_local_token_count: manifest.acceptance.require_exact_local_token_count,
            require_negative_complete_wire_token_delta: manifest
                .acceptance
                .require_negative_complete_wire_token_delta,
            require_round_trip: manifest.acceptance.require_round_trip,
            require_freshness_semantics: manifest.acceptance.require_freshness_semantics,
            require_range_identity: manifest.acceptance.require_range_identity,
            require_known_hash_deduplication: manifest.acceptance.require_known_hash_deduplication,
            maximum_additional_exact_resend_source_tokens: manifest
                .acceptance
                .maximum_additional_exact_resend_source_tokens,
            maximum_additional_overlapping_source_tokens: manifest
                .acceptance
                .maximum_additional_overlapping_source_tokens,
            non_dual_mode_requires_exact_host_version_evidence: manifest
                .acceptance
                .non_dual_mode_requires_exact_host_version_evidence,
            provider_values_must_remain_null_when_unavailable: manifest
                .acceptance
                .provider_values_must_remain_null_when_unavailable,
        },
        baseline,
        follow_up,
        candidates,
        decision: Decision {
            accepted_new_runtime_change: vec!["omit_task_fingerprint"],
            retained_existing_compactions: vec![
                "aligned_receipt_hash_table",
                "compact_fragment_metadata",
                "omit_empty_and_default_fields",
            ],
            host_scoped_opt_in: vec!["structured_mode on Codex CLI 0.144.1"],
            rejected_candidates: vec![
                "text_mode",
                "short_reason_codes",
                "omit_omission_details",
                "omit_default_meta",
                "compact_outline_tuples",
                "compact_tree_paths",
                "remove_tool_examples",
            ],
            global_result_mode: "dual",
            provider_input_conclusion: "unknown: no captured provider request frame permits attribution of a representation delta",
        },
        limitations: vec![
            "Local cl100k_base counts cover complete modeled JSON-RPC messages but are not provider billing counts.",
            "The fixture is deterministic and small; absolute deltas vary with result size, path lengths, and the selected evidence.",
            "Only one Codex CLI 0.144.1 task proves structured-only model consumption; no text-only real-host proof exists.",
            "Serialization-only candidates leave the frozen retrieval trajectory unchanged, but tool-description changes need controlled model evaluation.",
            "The follow-up detects exact content resends and overlapping ranges for this task; it does not establish behavior for every repository or task.",
        ],
    })
}

fn validate_manifest(manifest: &Manifest) -> AnyResult<()> {
    if manifest.schema_version != 1
        || manifest.experiment != "mcp-response-ablation-v1"
        || manifest.fixture.root.is_absolute()
        || manifest.fixture.task.trim().is_empty()
        || manifest.fixture.token_budget == 0
        || manifest.fixture.tokenizer != "cl100k_base"
        || !manifest.fixture.canonicalize_line_endings
        || manifest.host_compatibility_evidence.path.is_absolute()
        || manifest.host_compatibility_evidence.global_default != "dual"
    {
        return Err(invalid_data("invalid MCP response ablation manifest"));
    }
    for hash in [
        &manifest.fixture.tree_blake3,
        &manifest.host_compatibility_evidence.blake3,
    ] {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_data("manifest commitments must be BLAKE3 hex"));
        }
    }
    if !manifest.acceptance.require_exact_local_token_count
        || !manifest
            .acceptance
            .require_negative_complete_wire_token_delta
        || !manifest.acceptance.require_round_trip
        || !manifest.acceptance.require_freshness_semantics
        || !manifest.acceptance.require_range_identity
        || !manifest.acceptance.require_known_hash_deduplication
        || manifest
            .acceptance
            .maximum_additional_exact_resend_source_tokens
            != 0
        || manifest
            .acceptance
            .maximum_additional_overlapping_source_tokens
            != 0
        || !manifest
            .acceptance
            .non_dual_mode_requires_exact_host_version_evidence
        || !manifest
            .acceptance
            .provider_values_must_remain_null_when_unavailable
    {
        return Err(invalid_data("frozen acceptance gates were weakened"));
    }
    Ok(())
}

fn validate_host_matrix(manifest: &Manifest, matrix: &HostMatrix) -> AnyResult<bool> {
    if matrix.policy.global_default != manifest.host_compatibility_evidence.global_default {
        return Err(invalid_data(
            "host matrix default does not match the manifest",
        ));
    }
    Ok(matrix.host_observations.iter().any(|observation| {
        observation.host == "codex-cli"
            && observation.version.as_deref() == Some("0.144.1")
            && observation
                .modes
                .iter()
                .any(|mode| mode.mode == "structured" && mode.model_consumption == "proven")
    }))
}

fn validate_context_semantics(response: &ContextResponse) -> AnyResult<()> {
    if response.fragments.is_empty()
        || response.receipt.task_fingerprint.len() != 32
        || response.receipt.fragment_hashes.len() != response.fragments.len()
        || response.meta.repository_generation == 0
        || !response.meta.token_count_exact
    {
        return Err(invalid_data(
            "frozen context response lacks required semantics",
        ));
    }
    Ok(())
}

fn follow_up_metrics(first: &ContextResponse, second: &ContextResponse) -> FollowUp {
    let first_hashes = first
        .receipt
        .fragment_hashes
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let exact_resend_source_tokens = second
        .fragments
        .iter()
        .zip(&second.receipt.fragment_hashes)
        .filter(|(_, hash)| first_hashes.contains(hash.as_str()))
        .map(|(fragment, _)| fragment.token_count)
        .sum();
    let overlapping_source_tokens = second
        .fragments
        .iter()
        .filter(|second| {
            first.fragments.iter().any(|first| {
                first.path == second.path
                    && first.start_line <= second.end_line
                    && second.start_line <= first.end_line
            })
        })
        .map(|fragment| fragment.token_count)
        .sum();
    let known_hash_omission_count = second
        .omitted
        .iter()
        .filter(|omitted| omitted.reason == "known hash")
        .count();
    FollowUp {
        exact_resend_source_tokens,
        overlapping_source_tokens,
        known_hash_omission_count,
    }
}

fn measure(
    wire: &WireContext<'_>,
    call_request: &Value,
    response: &Value,
    mode: McpResultMode,
    source_tokens: usize,
    follow_up: FollowUp,
) -> AnyResult<Measurement> {
    let result = tool_result(response.clone(), mode)?;
    let call_result = json!({"jsonrpc": "2.0", "id": 2, "result": result});
    let tools_list_response = json!({
        "jsonrpc": "2.0", "id": 1, "result": {"tools": wire.catalog}
    });
    let count = |value: &Value| wire.tokenizer.count(&value.to_string());
    let serialized_result_tokens = count(&call_result);
    let complete_wire_tokens = count(&wire.initialize_request)
        + count(&wire.initialize_response)
        + count(&wire.initialized_notification)
        + count(&wire.tools_list_request)
        + count(&tools_list_response)
        + count(call_request)
        + serialized_result_tokens;
    Ok(Measurement {
        source_tokens,
        response_json_tokens: count(response),
        serialized_result_tokens,
        complete_wire_tokens,
        provider_native_input_tokens: None,
        exact_resend_source_tokens: follow_up.exact_resend_source_tokens,
        overlapping_source_tokens: follow_up.overlapping_source_tokens,
    })
}

#[allow(clippy::too_many_arguments)]
fn compare(
    wire: &WireContext<'_>,
    call_request: &Value,
    baseline_value: &Value,
    baseline_mode: McpResultMode,
    candidate_value: &Value,
    candidate_mode: McpResultMode,
    source_tokens: usize,
    follow_up: FollowUp,
    surface: &'static str,
) -> AnyResult<Comparison> {
    let baseline = measure(
        wire,
        call_request,
        baseline_value,
        baseline_mode,
        source_tokens,
        follow_up,
    )?;
    let candidate = measure(
        wire,
        call_request,
        candidate_value,
        candidate_mode,
        source_tokens,
        follow_up,
    )?;
    Ok(comparison(
        surface,
        mode_name(candidate_mode),
        baseline,
        candidate,
    ))
}

#[allow(clippy::too_many_arguments)]
fn compare_wire_contexts(
    baseline_wire: &WireContext<'_>,
    candidate_wire: &WireContext<'_>,
    call_request: &Value,
    response: &Value,
    mode: McpResultMode,
    source_tokens: usize,
    follow_up: FollowUp,
    surface: &'static str,
) -> AnyResult<Comparison> {
    let baseline = measure(
        baseline_wire,
        call_request,
        response,
        mode,
        source_tokens,
        follow_up,
    )?;
    let candidate = measure(
        candidate_wire,
        call_request,
        response,
        mode,
        source_tokens,
        follow_up,
    )?;
    Ok(comparison(surface, mode_name(mode), baseline, candidate))
}

fn comparison(
    surface: &'static str,
    mode: &'static str,
    baseline: Measurement,
    candidate: Measurement,
) -> Comparison {
    Comparison {
        surface,
        mode,
        response_json_token_delta: delta(
            candidate.response_json_tokens,
            baseline.response_json_tokens,
        ),
        serialized_result_token_delta: delta(
            candidate.serialized_result_tokens,
            baseline.serialized_result_tokens,
        ),
        complete_wire_token_delta: delta(
            candidate.complete_wire_tokens,
            baseline.complete_wire_tokens,
        ),
        baseline,
        candidate,
    }
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    id: &str,
    change: &'static str,
    measurements: Vec<Comparison>,
    round_trip: bool,
    freshness_semantics: bool,
    range_identity: bool,
    known_hash_deduplication: bool,
    model_behavior_evidence: &'static str,
    host_compatibility: &'static str,
    decision: &'static str,
    rationale: &'static str,
) -> CandidateReport {
    CandidateReport {
        id: id.to_owned(),
        change,
        measurements,
        round_trip,
        freshness_semantics,
        range_identity,
        known_hash_deduplication,
        model_behavior_evidence,
        host_compatibility,
        decision,
        rationale,
    }
}

fn validate_candidate_coverage(
    manifest: &Manifest,
    candidates: &[CandidateReport],
) -> AnyResult<()> {
    let expected = manifest
        .candidates
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let actual = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect::<BTreeSet<_>>();
    if expected != actual || expected.len() != manifest.candidates.len() {
        return Err(invalid_data(
            "report candidate set differs from the frozen manifest",
        ));
    }
    if candidates.iter().any(|candidate| {
        candidate.measurements.is_empty()
            || candidate.measurements.iter().any(|measurement| {
                measurement.baseline.provider_native_input_tokens.is_some()
                    || measurement.candidate.provider_native_input_tokens.is_some()
            })
    }) {
        return Err(invalid_data(
            "candidate measurements are incomplete or impute provider tokens",
        ));
    }
    Ok(())
}

fn validate_accepted_candidate(
    manifest: &Manifest,
    candidates: &[CandidateReport],
) -> AnyResult<()> {
    let accepted = candidates
        .iter()
        .find(|candidate| candidate.id == "omit_task_fingerprint")
        .ok_or_else(|| invalid_data("accepted candidate is missing"))?;
    if accepted.decision != "accepted"
        || !accepted.round_trip
        || !accepted.freshness_semantics
        || !accepted.range_identity
        || !accepted.known_hash_deduplication
        || accepted
            .measurements
            .iter()
            .any(|measurement| measurement.complete_wire_token_delta >= 0)
        || accepted.measurements.iter().any(|measurement| {
            measurement.candidate.exact_resend_source_tokens
                > measurement
                    .baseline
                    .exact_resend_source_tokens
                    .saturating_add(
                        manifest
                            .acceptance
                            .maximum_additional_exact_resend_source_tokens,
                    )
                || measurement.candidate.overlapping_source_tokens
                    > measurement
                        .baseline
                        .overlapping_source_tokens
                        .saturating_add(
                            manifest
                                .acceptance
                                .maximum_additional_overlapping_source_tokens,
                        )
        })
    {
        return Err(invalid_data(
            "accepted candidate did not pass every frozen gate",
        ));
    }
    Ok(())
}

fn with_task_fingerprint(mut value: Value, response: &ContextResponse) -> AnyResult<Value> {
    object_field_mut(&mut value, "receipt")?.insert(
        "task_fingerprint".into(),
        Value::String(response.receipt.task_fingerprint.clone()),
    );
    Ok(value)
}

fn expand_receipt_identities(value: &Value, response: &ContextResponse) -> AnyResult<Value> {
    let mut expanded = value.clone();
    let identities = response
        .fragments
        .iter()
        .zip(&response.receipt.fragment_hashes)
        .map(|(fragment, hash)| {
            json!({
                "path": fragment.path,
                "start_line": fragment.start_line,
                "end_line": fragment.end_line,
                "content_hash": hash,
            })
        })
        .collect();
    let receipt = object_field_mut(&mut expanded, "receipt")?;
    receipt.remove("fragment_hashes");
    receipt.insert("fragments".into(), Value::Array(identities));
    Ok(expanded)
}

fn expand_fragment_metadata(value: &Value, response: &ContextResponse) -> AnyResult<Value> {
    let mut expanded = value.clone();
    let fragments = array_field_mut(&mut expanded, "fragments")?;
    for (value, fragment) in fragments.iter_mut().zip(&response.fragments) {
        let object = value
            .as_object_mut()
            .ok_or_else(|| invalid_data("fragment is not an object"))?;
        object.insert(
            "content_hash".into(),
            Value::String(fragment.content_hash.clone()),
        );
        object.insert("score".into(), json!(fragment.score));
        object.insert("token_count".into(), json!(fragment.token_count));
    }
    Ok(expanded)
}

fn expand_context_defaults(value: &Value, response: &ContextResponse) -> AnyResult<Value> {
    let mut expanded = value.clone();
    let fragments = array_field_mut(&mut expanded, "fragments")?;
    for (value, fragment) in fragments.iter_mut().zip(&response.fragments) {
        if fragment.representation == "source" {
            value
                .as_object_mut()
                .ok_or_else(|| invalid_data("fragment is not an object"))?
                .insert("representation".into(), Value::String("source".into()));
        }
    }
    object_field_mut(&mut expanded, "meta")?.insert("next_cursor".into(), Value::Null);
    if response.omitted.is_empty() {
        expanded
            .as_object_mut()
            .ok_or_else(|| invalid_data("context response is not an object"))?
            .insert("omitted".into(), Value::Array(Vec::new()));
    }
    if response.warnings.is_empty() {
        expanded
            .as_object_mut()
            .ok_or_else(|| invalid_data("context response is not an object"))?
            .insert("warnings".into(), Value::Array(Vec::new()));
    }
    Ok(expanded)
}

fn expand_outline_defaults(mut value: Value) -> AnyResult<Value> {
    for file in array_field_mut(&mut value, "files")? {
        let file = file
            .as_object_mut()
            .ok_or_else(|| invalid_data("outline file is not an object"))?;
        file.entry("symbols")
            .or_insert_with(|| Value::Array(Vec::new()));
        file.entry("imports")
            .or_insert_with(|| Value::Array(Vec::new()));
    }
    object_field_mut(&mut value, "meta")?.insert("next_cursor".into(), Value::Null);
    Ok(value)
}

fn encode_short_reasons(mut value: Value) -> AnyResult<Value> {
    for fragment in array_field_mut(&mut value, "fragments")? {
        let object = fragment
            .as_object_mut()
            .ok_or_else(|| invalid_data("fragment is not an object"))?;
        let reason = object
            .get("reason")
            .and_then(|value| value.as_str())
            .ok_or_else(|| invalid_data("fragment reason is missing"))?;
        let encoded = reason
            .split("; ")
            .map(|part| match part {
                "symbol" => "s",
                "text" => "t",
                "reference" => "r",
                "focus" => "f",
                "import" => "i",
                "changed" => "c",
                _ => "?",
            })
            .collect::<Vec<_>>()
            .join(",");
        object.insert("reason".into(), Value::String(encoded));
    }
    Ok(value)
}

fn omit_default_meta(mut value: Value) -> AnyResult<Value> {
    let meta = object_field_mut(&mut value, "meta")?;
    meta.remove("freshness");
    meta.remove("token_count_exact");
    Ok(value)
}

fn compact_outline_tuples(mut value: Value) -> AnyResult<Value> {
    for file in array_field_mut(&mut value, "files")? {
        let file = file
            .as_object_mut()
            .ok_or_else(|| invalid_data("outline file is not an object"))?;
        if let Some(symbols) = file.get_mut("symbols").and_then(Value::as_array_mut) {
            for symbol in symbols {
                let object = symbol
                    .as_object()
                    .ok_or_else(|| invalid_data("outline symbol is not an object"))?;
                *symbol = Value::Array(
                    [
                        "name",
                        "kind",
                        "parent",
                        "signature",
                        "start_line",
                        "end_line",
                        "start_byte",
                        "end_byte",
                    ]
                    .into_iter()
                    .map(|field| object.get(field).cloned().unwrap_or(Value::Null))
                    .collect(),
                );
            }
        }
        if let Some(imports) = file.get_mut("imports").and_then(Value::as_array_mut) {
            for import in imports {
                let object = import
                    .as_object()
                    .ok_or_else(|| invalid_data("outline import is not an object"))?;
                *import = Value::Array(
                    ["raw_target", "resolved_path", "line"]
                        .into_iter()
                        .map(|field| object.get(field).cloned().unwrap_or(Value::Null))
                        .collect(),
                );
            }
        }
    }
    Ok(value)
}

fn compact_tree_paths(mut value: Value) -> AnyResult<Value> {
    for entry in array_field_mut(&mut value, "entries")? {
        let path = entry
            .as_object()
            .and_then(|object| object.get("path"))
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_data("tree entry path is missing"))?;
        *entry = Value::String(path.to_owned());
    }
    Ok(value)
}

fn remove_tool_examples(mut catalog: Value) -> AnyResult<Value> {
    let tools = catalog
        .as_array_mut()
        .ok_or_else(|| invalid_data("tool catalog is not an array"))?;
    for tool in tools {
        let description = tool
            .as_object_mut()
            .and_then(|object| object.get_mut("description"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| invalid_data("tool description is missing"))?;
        let compact = description
            .split_once(" Example:")
            .map_or(description, |(prefix, _)| prefix)
            .to_owned();
        tool.as_object_mut()
            .expect("validated tool object")
            .insert("description".into(), Value::String(compact));
    }
    Ok(catalog)
}

fn remove_field(mut value: Value, field: &str) -> AnyResult<Value> {
    value
        .as_object_mut()
        .ok_or_else(|| invalid_data("response is not an object"))?
        .remove(field);
    Ok(value)
}

fn object_field_mut<'a>(
    value: &'a mut Value,
    field: &str,
) -> AnyResult<&'a mut Map<String, Value>> {
    value
        .as_object_mut()
        .and_then(|object| object.get_mut(field))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid_data(&format!("object field {field} is missing")))
}

fn array_field_mut<'a>(value: &'a mut Value, field: &str) -> AnyResult<&'a mut Vec<Value>> {
    value
        .as_object_mut()
        .and_then(|object| object.get_mut(field))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid_data(&format!("array field {field} is missing")))
}

fn canonical_fixture_files(root: &Path) -> AnyResult<Vec<(String, Vec<u8>)>> {
    let mut paths = Vec::new();
    collect_files(root, root, &mut paths)?;
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(paths)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> AnyResult<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, canonical_json_or_text(fs::read(path)?)?));
        }
    }
    Ok(())
}

fn fixture_manifest_hash(files: &[(String, Vec<u8>)]) -> String {
    let manifest = files
        .iter()
        .map(|(path, bytes)| format!("{}  {path}\n", blake3::hash(bytes).to_hex()))
        .collect::<String>();
    blake3::hash(manifest.as_bytes()).to_hex().to_string()
}

fn materialize_fixture(root: &Path, files: &[(String, Vec<u8>)]) -> AnyResult<()> {
    for (relative, bytes) in files {
        let destination = root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, bytes)?;
    }
    Ok(())
}

fn canonical_json(bytes: Vec<u8>) -> AnyResult<Vec<u8>> {
    let normalized = canonical_json_or_text(bytes)?;
    serde_json::from_slice::<Value>(&normalized)?;
    Ok(normalized)
}

fn canonical_json_or_text(bytes: Vec<u8>) -> AnyResult<Vec<u8>> {
    let text = String::from_utf8(bytes)?;
    let normalized = text.replace("\r\n", "\n");
    if normalized.contains('\r') {
        return Err(invalid_data("fixture contains a lone carriage return"));
    }
    Ok(normalized.into_bytes())
}

fn mode_name(mode: McpResultMode) -> &'static str {
    match mode {
        McpResultMode::Dual => "dual",
        McpResultMode::Text => "text",
        McpResultMode::Structured => "structured",
    }
}

fn delta(candidate: usize, baseline: usize) -> i64 {
    i64::try_from(candidate).expect("token count fits i64")
        - i64::try_from(baseline).expect("token count fits i64")
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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

    #[tokio::test]
    async fn checked_report_matches_frozen_manifest_and_runtime() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let manifest: Manifest = read_json(&root.join("benchmarks/mcp_response_ablation.json"))
            .expect("frozen manifest");
        let report = generate(&manifest, &root).await.expect("valid report");
        let checked_report: Value =
            read_json(&root.join("benchmarks/reports/mcp-response-ablation-v1-2026-07-21.json"))
                .expect("checked report");

        assert_eq!(report.candidates.len(), manifest.candidates.len());
        assert_eq!(report.decision.global_result_mode, "dual");
        assert_eq!(report.decision.accepted_new_runtime_change.len(), 1);
        assert_eq!(report.follow_up.exact_resend_source_tokens, 0);
        assert_eq!(report.follow_up.overlapping_source_tokens, 14);
        assert_eq!(
            serde_json::to_value(report).expect("serialize generated report"),
            checked_report
        );
    }

    #[test]
    fn short_reason_candidate_preserves_a_decodable_string_but_not_readability() {
        let value = json!({"fragments": [
            {"reason": "symbol; text; reference"},
            {"reason": "focus; changed"}
        ]});
        let encoded = encode_short_reasons(value).expect("encoded reasons");

        assert_eq!(encoded["fragments"][0]["reason"], "s,t,r");
        assert_eq!(encoded["fragments"][1]["reason"], "f,c");
    }
}
