use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use leantoken::services::Services;
use leantoken::tokens::Tokenizer;
use leantoken::{
    Config, ContextRequest, ContextResponse, HandoffEvidence, HandoffManifest,
    HandoffManifestRequest, HandoffValidation, HandoffValidationStatus, HandoffWorkingTreeState,
    ReadRequest, ReadResponse,
};
use serde::Serialize;
use serde_json::Value;

const RUNS: usize = 21;
const SOURCE_SENTINEL: &str = "HANDOFF_SOURCE_SENTINEL_9d2f41";

type AnyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Serialize)]
struct BenchmarkReport {
    schema_version: u32,
    claim: &'static str,
    fixture: FixtureReport,
    correctness: CorrectnessReport,
    payload_tokens: PayloadReport,
    performance_ms: PerformanceReport,
    decision: DecisionReport,
    limitations: Vec<&'static str>,
}

#[derive(Serialize)]
struct FixtureReport {
    selected_fragments: usize,
    selected_paths: Vec<String>,
    source_tokens: usize,
    tokenizer: String,
    runs_per_arm: usize,
}

#[derive(Serialize)]
struct CorrectnessReport {
    selection_parity: bool,
    exact_evidence_parity: bool,
    exact_reread_hash_parity: bool,
    provenance_complete: bool,
    host_state_exact: bool,
    manifest_has_no_source_bodies: bool,
    deterministic_manifest: bool,
    response_token_accounting_exact: bool,
}

#[derive(Serialize)]
struct PayloadReport {
    normal_context_response: usize,
    handoff_context_response: usize,
    full_source_handoff: usize,
    standalone_manifest: usize,
    largest_exact_reread: usize,
    all_exact_rereads: usize,
    baseline_two_stage: usize,
    manifest_zero_rereads: usize,
    manifest_one_reread: usize,
    manifest_all_rereads: usize,
    zero_reread_savings: isize,
    one_reread_savings: isize,
    all_reread_savings: isize,
    small_context_probe: CrossoverProbe,
    medium_context_probe: CrossoverProbe,
}

#[derive(Serialize)]
struct CrossoverProbe {
    selected_fragments: usize,
    source_tokens: usize,
    baseline_two_stage: usize,
    manifest_zero_rereads: usize,
    manifest_one_reread: usize,
    zero_reread_savings: isize,
    one_reread_savings: isize,
}

#[derive(Serialize)]
struct PerformanceReport {
    normal_context_p50: f64,
    normal_context_p95: f64,
    handoff_context_p50: f64,
    handoff_context_p95: f64,
}

#[derive(Serialize)]
struct DecisionReport {
    adopt: bool,
    reason: String,
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let output = std::env::args_os().nth(1).map(PathBuf::from);
    let (temporary, services) = fixture().await?;
    let request = context_request();
    let handoff = handoff_request();
    let normal = services.context(request.clone()).await?;
    let with_manifest = services
        .context_with_handoff(request.clone(), handoff.clone())
        .await?;
    let manifest = with_manifest
        .handoff_manifest
        .clone()
        .ok_or("handoff response omitted manifest")?;

    let selection_parity = fragment_identities(&normal) == fragment_identities(&with_manifest);
    let exact_evidence_parity =
        evidence_identities(&manifest.evidence) == fragment_identities(&normal);
    let provenance_complete = manifest.repository_id == normal.meta.repository_id
        && manifest.repository_generation == normal.meta.repository_generation
        && manifest.commit_revision.is_some()
        && manifest.working_tree_state == HandoffWorkingTreeState::Clean;
    let host_state_exact = manifest.summary == "Continue the routing policy implementation"
        && manifest.validations == handoff.validations
        && manifest.assumptions == handoff.assumptions
        && manifest.open_questions == handoff.open_questions
        && manifest.negative_evidence == handoff.negative_evidence
        && manifest.avoid_rules == handoff.avoid_rules;

    let tokenizer = Tokenizer::default();
    let manifest_json = serde_json::to_string(&manifest)?;
    let manifest_has_no_source_bodies =
        !manifest_json.contains(SOURCE_SENTINEL) && !manifest_json.contains("\"content\"");
    let full_source_handoff = full_source_handoff(&manifest, &normal)?;
    let full_source_json = serde_json::to_string(&full_source_handoff)?;
    if !full_source_json.contains(SOURCE_SENTINEL) {
        return Err("full-source baseline lost the fixture sentinel".into());
    }

    let reads = exact_rereads(&services, &manifest.evidence).await?;
    let exact_reread_hash_parity = reads
        .iter()
        .zip(&manifest.evidence)
        .all(|(read, evidence)| {
            read.path == evidence.path
                && read.returned_start_line == evidence.start_line
                && read.returned_end_line == evidence.end_line
                && read.content_hash == evidence.content_hash
                && !read.truncated
        });

    let normal_tokens = serialized_tokens(&normal, tokenizer)?;
    let handoff_response_tokens = serialized_tokens(&with_manifest, tokenizer)?;
    let full_source_tokens = serialized_value_tokens(&full_source_handoff, tokenizer)?;
    let manifest_tokens = serialized_tokens(&manifest, tokenizer)?;
    let reread_tokens = reads
        .iter()
        .map(|read| serialized_tokens(read, tokenizer))
        .collect::<AnyResult<Vec<_>>>()?;
    let largest_reread = reread_tokens.iter().copied().max().unwrap_or(0);
    let all_rereads = reread_tokens.iter().sum::<usize>();
    let baseline_two_stage = normal_tokens + full_source_tokens;
    let manifest_zero_rereads = handoff_response_tokens + manifest_tokens;
    let manifest_one_reread = manifest_zero_rereads + largest_reread;
    let manifest_all_rereads = manifest_zero_rereads + all_rereads;
    let small_context_probe =
        crossover_probe(&services, small_context_request(), &handoff, tokenizer).await?;
    let medium_context_probe =
        crossover_probe(&services, medium_context_request(), &handoff, tokenizer).await?;

    let deterministic_manifest = {
        let repeated = services
            .context_with_handoff(request.clone(), handoff.clone())
            .await?
            .handoff_manifest
            .ok_or("repeated response omitted manifest")?;
        normalize_manifest(manifest.clone()) == normalize_manifest(repeated)
    };
    let response_token_accounting_exact = response_accounting_exact(&normal, tokenizer)?
        && response_accounting_exact(&with_manifest, tokenizer)?
        && reads
            .iter()
            .map(|read| response_accounting_exact(read, tokenizer))
            .collect::<AnyResult<Vec<_>>>()?
            .into_iter()
            .all(std::convert::identity);

    let (normal_timings, handoff_timings) = measure_context(&services, &request, &handoff).await?;
    let correctness = CorrectnessReport {
        selection_parity,
        exact_evidence_parity,
        exact_reread_hash_parity,
        provenance_complete,
        host_state_exact,
        manifest_has_no_source_bodies,
        deterministic_manifest,
        response_token_accounting_exact,
    };
    let correctness_passed = correctness.selection_parity
        && correctness.exact_evidence_parity
        && correctness.exact_reread_hash_parity
        && correctness.provenance_complete
        && correctness.host_state_exact
        && correctness.manifest_has_no_source_bodies
        && correctness.deterministic_manifest
        && correctness.response_token_accounting_exact;
    let adopt = correctness_passed
        && normal.fragments.len() == 8
        && manifest_zero_rereads < baseline_two_stage
        && manifest_one_reread < baseline_two_stage;
    let reason = if adopt {
        "Adopt for broad handoffs: exact evidence and provenance are preserved, and the eight-fragment complete two-stage payload is smaller with zero or one exact reread; three- and six-fragment crossover probes remain disclosed."
    } else {
        "Reject: correctness or the zero/one-reread complete-payload gate did not pass."
    }
    .to_owned();

    let report = BenchmarkReport {
        schema_version: 1,
        claim: "For an eight-fragment context-to-executor workflow where the recipient needs at most one selected fragment, a coordinate-and-hash manifest reduces complete serialized payload versus copying all selected source while preserving exact provenance; three- and six-fragment crossover probes are reported separately.",
        fixture: FixtureReport {
            selected_fragments: normal.fragments.len(),
            selected_paths: normal
                .fragments
                .iter()
                .map(|fragment| fragment.path.clone())
                .collect(),
            source_tokens: normal.meta.source_tokens,
            tokenizer: tokenizer.name().into(),
            runs_per_arm: RUNS,
        },
        correctness,
        payload_tokens: PayloadReport {
            normal_context_response: normal_tokens,
            handoff_context_response: handoff_response_tokens,
            full_source_handoff: full_source_tokens,
            standalone_manifest: manifest_tokens,
            largest_exact_reread: largest_reread,
            all_exact_rereads: all_rereads,
            baseline_two_stage,
            manifest_zero_rereads,
            manifest_one_reread,
            manifest_all_rereads,
            zero_reread_savings: savings(baseline_two_stage, manifest_zero_rereads),
            one_reread_savings: savings(baseline_two_stage, manifest_one_reread),
            all_reread_savings: savings(baseline_two_stage, manifest_all_rereads),
            small_context_probe,
            medium_context_probe,
        },
        performance_ms: PerformanceReport {
            normal_context_p50: percentile(&normal_timings, 0.50),
            normal_context_p95: percentile(&normal_timings, 0.95),
            handoff_context_p50: percentile(&handoff_timings, 0.50),
            handoff_context_p95: percentile(&handoff_timings, 0.95),
        },
        decision: DecisionReport { adopt, reason },
        limitations: vec![
            "The fixture is synthetic and validates mechanics and payload economics, not task success or repository-scale retrieval quality.",
            "The all-reread result is a disclosed worst case and is not an adoption gate.",
            "Small contexts can cost more after an exact reread; hosts should request manifests for genuine multi-fragment handoffs, not routine context calls.",
            "Caller-reported validations are transported exactly but are not executed by LeanToken.",
            "Timing is descriptive and is not a correctness gate.",
        ],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, format!("{json}\n"))?;
    } else {
        println!("{json}");
    }
    drop(temporary);
    if !adopt {
        return Err("handoff manifest benchmark rejected the feature".into());
    }
    Ok(())
}

async fn fixture() -> AnyResult<(tempfile::TempDir, Services)> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    fs::create_dir_all(repository.join("src"))?;
    fs::create_dir_all(repository.join("tests"))?;
    fs::write(
        repository.join("src/router.rs"),
        format!(
            "pub fn select_route(method: &str, path: &str) -> &'static str {{\n    // {SOURCE_SENTINEL}: routing evidence must never enter the manifest.\n    match (method, path) {{\n        (\"GET\", \"/health\") => \"health\",\n        (\"POST\", \"/sessions\") => \"create_session\",\n        (\"DELETE\", \"/sessions/current\") => \"delete_session\",\n        _ => \"not_found\",\n    }}\n}}\n\npub fn dispatch(method: &str, path: &str) -> String {{\n    format!(\"handler={{}}\", select_route(method, path))\n}}\n"
        ),
    )?;
    fs::write(
        repository.join("src/policy.rs"),
        "pub fn authorize(route: &str, roles: &[&str]) -> bool {\n    match route {\n        \"health\" => true,\n        \"create_session\" => roles.contains(&\"member\"),\n        \"delete_session\" => roles.contains(&\"admin\"),\n        _ => false,\n    }\n}\n\npub fn audit_label(route: &str, allowed: bool) -> String {\n    let outcome = if allowed { \"allowed\" } else { \"denied\" };\n    format!(\"route={route} outcome={outcome}\")\n}\n",
    )?;
    fs::write(
        repository.join("src/session.rs"),
        "pub struct Session {\n    pub id: String,\n    pub member: String,\n    pub active: bool,\n}\n\npub fn create_session(id: &str, member: &str) -> Session {\n    Session { id: id.into(), member: member.into(), active: true }\n}\n\npub fn close_session(session: &mut Session) -> bool {\n    if !session.active {\n        return false;\n    }\n    session.active = false;\n    true\n}\n",
    )?;
    fs::write(
        repository.join("src/audit.rs"),
        "pub struct AuditEvent {\n    pub route: String,\n    pub outcome: String,\n    pub actor: String,\n}\n\npub fn record_event(route: &str, allowed: bool, actor: &str) -> AuditEvent {\n    AuditEvent {\n        route: route.into(),\n        outcome: if allowed { \"allowed\" } else { \"denied\" }.into(),\n        actor: actor.into(),\n    }\n}\n\npub fn render_event(event: &AuditEvent) -> String {\n    format!(\"actor={} route={} outcome={}\", event.actor, event.route, event.outcome)\n}\n",
    )?;
    fs::write(
        repository.join("src/rate_limit.rs"),
        "pub struct RateLimit {\n    pub capacity: u32,\n    pub remaining: u32,\n}\n\npub fn new_limit(capacity: u32) -> RateLimit {\n    RateLimit { capacity, remaining: capacity }\n}\n\npub fn consume(limit: &mut RateLimit, units: u32) -> bool {\n    if units > limit.remaining {\n        return false;\n    }\n    limit.remaining -= units;\n    true\n}\n\npub fn reset(limit: &mut RateLimit) {\n    limit.remaining = limit.capacity;\n}\n",
    )?;
    fs::write(
        repository.join("tests/router_test.rs"),
        "use crate::policy::{audit_label, authorize};\nuse crate::router::{dispatch, select_route};\n\n#[test]\nfn routes_and_policy_remain_aligned() {\n    let route = select_route(\"POST\", \"/sessions\");\n    assert_eq!(route, \"create_session\");\n    assert!(authorize(route, &[\"member\"]));\n    assert_eq!(dispatch(\"GET\", \"/health\"), \"handler=health\");\n    assert_eq!(audit_label(route, true), \"route=create_session outcome=allowed\");\n}\n",
    )?;
    fs::write(
        repository.join("tests/policy_test.rs"),
        "use crate::audit::{record_event, render_event};\nuse crate::policy::authorize;\nuse crate::session::{close_session, create_session};\n\n#[test]\nfn sessions_policy_and_audit_remain_aligned() {\n    let mut session = create_session(\"session-1\", \"member-1\");\n    assert!(authorize(\"create_session\", &[\"member\"]));\n    let event = record_event(\"create_session\", true, &session.member);\n    assert_eq!(render_event(&event), \"actor=member-1 route=create_session outcome=allowed\");\n    assert!(close_session(&mut session));\n    assert!(!session.active);\n}\n",
    )?;
    fs::write(
        repository.join("tests/session_test.rs"),
        "use crate::rate_limit::{consume, new_limit, reset};\nuse crate::session::{close_session, create_session};\n\n#[test]\nfn session_lifecycle_respects_rate_limits() {\n    let mut session = create_session(\"session-2\", \"member-2\");\n    let mut limit = new_limit(3);\n    assert!(consume(&mut limit, 2));\n    assert!(!consume(&mut limit, 2));\n    reset(&mut limit);\n    assert!(consume(&mut limit, 3));\n    assert!(close_session(&mut session));\n    assert!(!close_session(&mut session));\n}\n",
    )?;
    run_git(&repository, &["init", "--quiet"])?;
    run_git(
        &repository,
        &["config", "user.email", "benchmark@example.com"],
    )?;
    run_git(&repository, &["config", "user.name", "LeanToken Benchmark"])?;
    run_git(&repository, &["add", "."])?;
    run_git(&repository, &["commit", "--quiet", "-m", "fixture"])?;

    let config = Config::discover(
        &repository,
        Some(temporary.path().join("cache/index.sqlite")),
    )?;
    let services = Services::open(config)?;
    services.index(false).await?;
    Ok((temporary, services))
}

fn context_request() -> ContextRequest {
    ContextRequest {
        task: "Trace select_route, authorize, dispatch, audit_label, and their owner test before continuing the routing policy implementation".into(),
        token_budget: 4_000,
        include_paths: Vec::new(),
        must_include_paths: vec![
            "src/router.rs".into(),
            "src/policy.rs".into(),
            "src/session.rs".into(),
            "src/audit.rs".into(),
            "src/rate_limit.rs".into(),
            "tests/router_test.rs".into(),
            "tests/policy_test.rs".into(),
            "tests/session_test.rs".into(),
        ],
        must_include_symbols: Vec::new(),
        required_evidence: Vec::new(),
        max_fragments: Some(8),
        plan_only: false,
        focus_paths: vec!["src".into(), "tests".into()],
        strict_focus_paths: false,
        minimum_fragments_per_focus_path: None,
        focus_symbols: vec![
            "select_route".into(),
            "authorize".into(),
            "create_session".into(),
            "record_event".into(),
            "consume".into(),
            "routes_and_policy_remain_aligned".into(),
            "sessions_policy_and_audit_remain_aligned".into(),
            "session_lifecycle_respects_rate_limits".into(),
        ],
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        receipt_id: None,
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        verbose_diagnostics: false,
    }
}

fn small_context_request() -> ContextRequest {
    let mut request = context_request();
    request.include_paths = vec![
        "src/router.rs".into(),
        "src/policy.rs".into(),
        "tests/router_test.rs".into(),
    ];
    request.must_include_paths = request.include_paths.clone();
    request.max_fragments = Some(3);
    request.focus_symbols = vec![
        "select_route".into(),
        "authorize".into(),
        "routes_and_policy_remain_aligned".into(),
    ];
    request
}

fn medium_context_request() -> ContextRequest {
    let mut request = context_request();
    request.include_paths = vec![
        "src/router.rs".into(),
        "src/policy.rs".into(),
        "src/session.rs".into(),
        "src/audit.rs".into(),
        "tests/router_test.rs".into(),
        "tests/policy_test.rs".into(),
    ];
    request.must_include_paths = request.include_paths.clone();
    request.max_fragments = Some(6);
    request.focus_symbols = vec![
        "select_route".into(),
        "authorize".into(),
        "create_session".into(),
        "record_event".into(),
        "routes_and_policy_remain_aligned".into(),
        "sessions_policy_and_audit_remain_aligned".into(),
    ];
    request
}

fn handoff_request() -> HandoffManifestRequest {
    HandoffManifestRequest {
        summary: Some("Continue the routing policy implementation".into()),
        validations: vec![HandoffValidation {
            command: "cargo test routes_and_policy_remain_aligned".into(),
            status: HandoffValidationStatus::Passed,
            summary: Some("owner test passed before handoff".into()),
        }],
        assumptions: vec!["route names remain stable API values".into()],
        open_questions: vec!["should delete_session accept a support role?".into()],
        negative_evidence: vec!["no second policy owner was found".into()],
        avoid_rules: vec!["do not copy complete source files into the handoff".into()],
    }
}

fn full_source_handoff(manifest: &HandoffManifest, response: &ContextResponse) -> AnyResult<Value> {
    let fragments = response
        .fragments
        .iter()
        .map(|fragment| {
            (
                (
                    fragment.path.as_str(),
                    fragment.start_line,
                    fragment.end_line,
                    fragment.content_hash.as_str(),
                ),
                fragment.content.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut value = serde_json::to_value(manifest)?;
    let evidence = value
        .get_mut("evidence")
        .and_then(Value::as_array_mut)
        .ok_or("manifest evidence is not an array")?;
    for item in evidence {
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .ok_or("evidence path missing")?;
        let start = item
            .get("start_line")
            .and_then(Value::as_u64)
            .ok_or("evidence start missing")? as usize;
        let end = item
            .get("end_line")
            .and_then(Value::as_u64)
            .ok_or("evidence end missing")? as usize;
        let hash = item
            .get("content_hash")
            .and_then(Value::as_str)
            .ok_or("evidence hash missing")?;
        let content = fragments
            .get(&(path, start, end, hash))
            .ok_or("baseline fragment did not match manifest evidence")?
            .to_string();
        item.as_object_mut()
            .ok_or("evidence item is not an object")?
            .insert("content".into(), Value::String(content));
    }
    Ok(value)
}

async fn exact_rereads(
    services: &Services,
    evidence: &[HandoffEvidence],
) -> AnyResult<Vec<ReadResponse>> {
    let mut responses = Vec::with_capacity(evidence.len());
    for evidence in evidence {
        responses.push(
            services
                .read(ReadRequest {
                    path: evidence.path.clone(),
                    start_line: Some(evidence.start_line),
                    end_line: Some(evidence.end_line),
                    symbol: None,
                    heading: None,
                    heading_occurrence: None,
                    continuation_cursor: None,
                    max_tokens: Some(32_000),
                    expected_hash: None,
                    delta: false,
                    receipt_id: None,
                })
                .await?,
        );
    }
    Ok(responses)
}

async fn crossover_probe(
    services: &Services,
    request: ContextRequest,
    handoff: &HandoffManifestRequest,
    tokenizer: Tokenizer,
) -> AnyResult<CrossoverProbe> {
    let normal = services.context(request.clone()).await?;
    let with_manifest = services
        .context_with_handoff(request, handoff.clone())
        .await?;
    let manifest = with_manifest
        .handoff_manifest
        .as_ref()
        .ok_or("small-context response omitted manifest")?;
    if fragment_identities(&normal) != fragment_identities(&with_manifest)
        || evidence_identities(&manifest.evidence) != fragment_identities(&normal)
    {
        return Err("small-context evidence parity failed".into());
    }
    let full_source = full_source_handoff(manifest, &normal)?;
    let reads = exact_rereads(services, &manifest.evidence).await?;
    let baseline =
        serialized_tokens(&normal, tokenizer)? + serialized_value_tokens(&full_source, tokenizer)?;
    let zero =
        serialized_tokens(&with_manifest, tokenizer)? + serialized_tokens(manifest, tokenizer)?;
    let largest_read = reads
        .iter()
        .map(|read| serialized_tokens(read, tokenizer))
        .collect::<AnyResult<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    Ok(CrossoverProbe {
        selected_fragments: normal.fragments.len(),
        source_tokens: normal.meta.source_tokens,
        baseline_two_stage: baseline,
        manifest_zero_rereads: zero,
        manifest_one_reread: zero + largest_read,
        zero_reread_savings: savings(baseline, zero),
        one_reread_savings: savings(baseline, zero + largest_read),
    })
}

async fn measure_context(
    services: &Services,
    request: &ContextRequest,
    handoff: &HandoffManifestRequest,
) -> AnyResult<(Vec<f64>, Vec<f64>)> {
    let mut normal = Vec::with_capacity(RUNS);
    let mut manifests = Vec::with_capacity(RUNS);
    for run in 0..RUNS {
        if run % 2 == 0 {
            normal.push(measure_normal(services, request).await?);
            manifests.push(measure_handoff(services, request, handoff).await?);
        } else {
            manifests.push(measure_handoff(services, request, handoff).await?);
            normal.push(measure_normal(services, request).await?);
        }
    }
    Ok((normal, manifests))
}

async fn measure_normal(services: &Services, request: &ContextRequest) -> AnyResult<f64> {
    let started = Instant::now();
    let response = services.context(request.clone()).await?;
    if response.fragments.is_empty() {
        return Err("timed normal context returned no fragments".into());
    }
    Ok(started.elapsed().as_secs_f64() * 1_000.0)
}

async fn measure_handoff(
    services: &Services,
    request: &ContextRequest,
    handoff: &HandoffManifestRequest,
) -> AnyResult<f64> {
    let started = Instant::now();
    let response = services
        .context_with_handoff(request.clone(), handoff.clone())
        .await?;
    if response.handoff_manifest.is_none() {
        return Err("timed handoff context omitted its manifest".into());
    }
    Ok(started.elapsed().as_secs_f64() * 1_000.0)
}

fn fragment_identities(response: &ContextResponse) -> Vec<(String, usize, usize, String)> {
    let mut identities = response
        .fragments
        .iter()
        .map(|fragment| {
            (
                fragment.path.clone(),
                fragment.start_line,
                fragment.end_line,
                fragment.content_hash.clone(),
            )
        })
        .collect::<Vec<_>>();
    identities.sort();
    identities
}

fn evidence_identities(evidence: &[HandoffEvidence]) -> Vec<(String, usize, usize, String)> {
    evidence
        .iter()
        .map(|evidence| {
            (
                evidence.path.clone(),
                evidence.start_line,
                evidence.end_line,
                evidence.content_hash.clone(),
            )
        })
        .collect()
}

fn normalize_manifest(mut manifest: HandoffManifest) -> HandoffManifest {
    manifest.receipt_id = None;
    manifest
}

fn serialized_tokens(value: &impl Serialize, tokenizer: Tokenizer) -> AnyResult<usize> {
    Ok(tokenizer.count(&serde_json::to_string(value)?))
}

fn serialized_value_tokens(value: &Value, tokenizer: Tokenizer) -> AnyResult<usize> {
    serialized_tokens(value, tokenizer)
}

fn response_accounting_exact<T>(response: &T, tokenizer: Tokenizer) -> AnyResult<bool>
where
    T: Serialize + Clone + ResponseAccounting,
{
    let mut countable = response.clone();
    let expected = countable.total_response_tokens();
    countable.clear_response_accounting();
    Ok(expected == serialized_tokens(&countable, tokenizer)?)
}

trait ResponseAccounting {
    fn total_response_tokens(&self) -> usize;
    fn clear_response_accounting(&mut self);
}

impl ResponseAccounting for ContextResponse {
    fn total_response_tokens(&self) -> usize {
        self.meta.total_response_tokens
    }

    fn clear_response_accounting(&mut self) {
        self.meta.protocol_tokens = 0;
        self.meta.path_and_metadata_tokens = 0;
        self.meta.total_response_tokens = 0;
        self.meta.total_response_tokens = 0;
    }
}

impl ResponseAccounting for ReadResponse {
    fn total_response_tokens(&self) -> usize {
        self.meta.total_response_tokens
    }

    fn clear_response_accounting(&mut self) {
        self.meta.protocol_tokens = 0;
        self.meta.path_and_metadata_tokens = 0;
        self.meta.total_response_tokens = 0;
        self.meta.total_response_tokens = 0;
    }
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let index = ((values.len() - 1) as f64 * quantile).ceil() as usize;
    values[index]
}

fn savings(baseline: usize, candidate: usize) -> isize {
    baseline as isize - candidate as isize
}

fn run_git(root: &Path, args: &[&str]) -> AnyResult<()> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )
    .into())
}
