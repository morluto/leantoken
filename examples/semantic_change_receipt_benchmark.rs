use std::{
    collections::BTreeSet,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use clap::Parser;
use leantoken::{
    Config, ContextRequest, ContextResponse, ContextWorkflow, DiffConfigurationChangeKind,
    DiffOwnerTestStatus, DiffSymbolChange, DiffSymbolChangeKind, DiffSymbolModification,
    IndexConsistency, services::Services, tokens::Tokenizer,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "target/semantic_change_receipt_report.json")]
    output: PathBuf,
    #[arg(long, default_value_t = 9)]
    iterations: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    leantoken_version: &'static str,
    tokenizer: &'static str,
    harness_revision: Option<String>,
    harness_tracked_worktree_dirty: Option<bool>,
    decision: &'static str,
    gates: Gates,
    classification: Classification,
    payload: Payload,
    latency_ms: Latency,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Gates {
    exact_truth_set: bool,
    no_configuration_values_exposed: bool,
    owner_test_statuses_correct: bool,
    bounded_payload_overhead: bool,
    bounded_p95_latency: bool,
}

#[derive(Debug, Serialize)]
struct Classification {
    expected_items: usize,
    returned_items: usize,
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    precision: f64,
    recall: f64,
}

#[derive(Debug, Serialize)]
struct Payload {
    response_tokens_with_receipt: usize,
    response_tokens_without_receipt: usize,
    receipt_overhead_tokens: usize,
    receipt_overhead_fraction: f64,
}

#[derive(Debug, Serialize)]
struct Latency {
    iterations: usize,
    min: f64,
    p50: f64,
    p95: f64,
    max: f64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if args.iterations == 0 {
        return Err("--iterations must be greater than zero".into());
    }

    let repository = PathBuf::from(env!("LEANTOKEN_REPOSITORY_ROOT"));
    let harness_revision = git_output(&repository, &["rev-parse", "HEAD"]);
    let harness_tracked_worktree_dirty = git_output(
        &repository,
        &["status", "--porcelain=v1", "--untracked-files=no"],
    )
    .map(|status| !status.trim().is_empty());
    let fixture = Fixture::new().await?;
    let mut latencies = Vec::with_capacity(args.iterations);
    let mut final_response = None;
    for _ in 0..args.iterations {
        let started = Instant::now();
        let response = fixture.review().await?;
        latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
        final_response = Some(response);
    }
    let response = final_response.expect("positive iteration count");
    let semantic = response
        .diff_scope
        .as_ref()
        .and_then(|scope| scope.evidence.as_ref())
        .and_then(|evidence| evidence.semantic_change.as_ref())
        .ok_or("semantic receipt missing")?;

    let expected = expected_truth();
    let returned = returned_truth(&semantic.symbol_changes, &semantic.configuration_changes);
    let true_positives = expected.intersection(&returned).count();
    let false_positives = returned.difference(&expected).count();
    let false_negatives = expected.difference(&returned).count();
    let precision = ratio(true_positives, true_positives + false_positives);
    let recall = ratio(true_positives, true_positives + false_negatives);
    let classification = Classification {
        expected_items: expected.len(),
        returned_items: returned.len(),
        true_positives,
        false_positives,
        false_negatives,
        precision,
        recall,
    };

    let serialized_semantic = serde_json::to_string(semantic)?;
    let no_configuration_values_exposed = !serialized_semantic.contains("base-secret-value")
        && !serialized_semantic.contains("head-secret-value");
    let owner_test_statuses_correct = semantic.owner_tests.iter().any(|coverage| {
        coverage.changed_path == "src/lib.rs"
            && coverage.status == DiffOwnerTestStatus::Found
            && coverage.paths == ["tests/lib_test.rs"]
    }) && semantic.owner_tests.iter().any(|coverage| {
        coverage.changed_path == "package.json" && coverage.status == DiffOwnerTestStatus::Missing
    });
    let tokenizer = Tokenizer::default();
    let response_tokens_with_receipt = serialized_tokens(&tokenizer, &response)?;
    let mut without_receipt = response.clone();
    if let Some(evidence) = without_receipt
        .diff_scope
        .as_mut()
        .and_then(|scope| scope.evidence.as_mut())
    {
        evidence.semantic_change = None;
    }
    let response_tokens_without_receipt = serialized_tokens(&tokenizer, &without_receipt)?;
    let receipt_overhead_tokens =
        response_tokens_with_receipt.saturating_sub(response_tokens_without_receipt);
    let payload = Payload {
        response_tokens_with_receipt,
        response_tokens_without_receipt,
        receipt_overhead_tokens,
        receipt_overhead_fraction: ratio(receipt_overhead_tokens, response_tokens_without_receipt),
    };
    latencies.sort_by(f64::total_cmp);
    let latency = Latency {
        iterations: latencies.len(),
        min: latencies[0],
        p50: percentile(&latencies, 0.50),
        p95: percentile(&latencies, 0.95),
        max: latencies[latencies.len() - 1],
    };
    let gates = Gates {
        exact_truth_set: false_positives == 0 && false_negatives == 0,
        no_configuration_values_exposed,
        owner_test_statuses_correct,
        bounded_payload_overhead: receipt_overhead_tokens <= 1_000,
        bounded_p95_latency: latency.p95 <= 2_000.0,
    };
    let adopted = gates.exact_truth_set
        && gates.no_configuration_values_exposed
        && gates.owner_test_statuses_correct
        && gates.bounded_payload_overhead
        && gates.bounded_p95_latency;
    let report = Report {
        schema_version: 1,
        leantoken_version: env!("CARGO_PKG_VERSION"),
        tokenizer: tokenizer.name(),
        harness_revision,
        harness_tracked_worktree_dirty,
        decision: if adopted { "adopt" } else { "reject" },
        gates,
        classification,
        payload,
        latency_ms: latency,
        limitations: vec![
            "This deterministic fixture validates receipt classification, not downstream model task success.",
            "Rename classification requires a unique normalized body fingerprint and intentionally under-classifies ambiguous cases.",
            "Configuration classification is limited to recognized JSON configuration filenames and emits key paths without values.",
            "Owner-test discovery remains a filename heuristic and reports unknown when its bounded scan truncates.",
        ],
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, format!("{json}\n"))?;
    println!("{json}");
    if !adopted {
        return Err("semantic change receipt adoption gates failed".into());
    }
    Ok(())
}

struct Fixture {
    _root: tempfile::TempDir,
    _database: tempfile::TempDir,
    services: Services,
    base: String,
    head: String,
}

impl Fixture {
    async fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let database = tempfile::tempdir()?;
        fs::create_dir_all(root.path().join("src"))?;
        fs::create_dir_all(root.path().join("tests"))?;
        fs::write(
            root.path().join("src/lib.rs"),
            "pub fn contract(value: i32) -> i32 {\n    value + 1\n}\n\nfn body_only(value: i32) -> i32 {\n    value + 1\n}\n\npub fn old_name(value: i32) -> i32 {\n    value * 2\n}\n\nfn removed() -> bool {\n    true\n}\n\nfn stable() -> bool {\n    true\n}\n",
        )?;
        fs::write(
            root.path().join("tests/lib_test.rs"),
            "#[test]\nfn contract_works() {\n    assert_eq!(crate::contract(1), 2);\n}\n",
        )?;
        fs::write(
            root.path().join("src/deleted.rs"),
            "pub fn deleted_file_symbol() -> bool {\n    true\n}\n",
        )?;
        fs::write(
            root.path().join("package.json"),
            r#"{"secret":"base-secret-value","removed":true,"nested":{"stable":1}}"#,
        )?;
        init_git(root.path())?;
        let base = required_git_output(root.path(), &["rev-parse", "HEAD"])?;

        fs::write(
            root.path().join("src/lib.rs"),
            "pub fn contract(value: i64) -> i64 {\n    value + 1\n}\n\nfn body_only(value: i32) -> i32 {\n    value + 2\n}\n\npub fn new_name(value: i32) -> i32 {\n    value * 2\n}\n\npub fn added() -> bool {\n    false\n}\n\nfn stable() -> bool {\n    true\n}\n",
        )?;
        fs::write(
            root.path().join("package.json"),
            r#"{"secret":"head-secret-value","added":false,"nested":{"stable":1}}"#,
        )?;
        fs::remove_file(root.path().join("src/deleted.rs"))?;
        fs::write(
            root.path().join("src/created.rs"),
            "pub fn created_file_symbol() -> bool {\n    true\n}\n",
        )?;
        run_git(root.path(), &["add", "."])?;
        run_git(root.path(), &["commit", "-m", "semantic changes"])?;
        let head = required_git_output(root.path(), &["rev-parse", "HEAD"])?;

        let config = Config::discover(root.path(), Some(database.path().join("index.sqlite")))?;
        let services = Services::open(config)?;
        services.index(false).await?;
        Ok(Self {
            _root: root,
            _database: database,
            services,
            base,
            head,
        })
    }

    async fn review(&self) -> Result<ContextResponse, Box<dyn Error>> {
        Ok(self
            .services
            .context_with_workflow_consistency_cancellable(
                ContextRequest {
                    task: "review public contracts, configuration, and owner tests".into(),
                    token_budget: 2_000,
                    include_paths: Vec::new(),
                    must_include_paths: Vec::new(),
                    must_include_symbols: Vec::new(),
                    required_evidence: Vec::new(),
                    max_fragments: None,
                    plan_only: false,
                    focus_paths: Vec::new(),
                    strict_focus_paths: false,
                    minimum_fragments_per_focus_path: None,
                    focus_symbols: Vec::new(),
                    exclude_paths: Vec::new(),
                    known_hashes: Vec::new(),
                    receipt_id: None,
                    prior_repository_generation: None,
                    base_revision: Some(format!("{}..{}", self.base, self.head)),
                    changed_paths: Vec::new(),
                    strict_changed_paths: true,
                    verbose_diagnostics: false,
                },
                ContextWorkflow::Review,
                IndexConsistency::IndexedGeneration,
                CancellationToken::new(),
            )
            .await?)
    }
}

fn expected_truth() -> BTreeSet<String> {
    [
        "symbol:modified:contract:signature_changed",
        "symbol:modified:body_only:body_only",
        "symbol:renamed:old_name->new_name",
        "symbol:removed:removed",
        "symbol:added:added",
        "symbol:removed:deleted_file_symbol",
        "symbol:added:created_file_symbol",
        "config:modified:/secret",
        "config:removed:/removed",
        "config:added:/added",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn returned_truth(
    symbols: &[DiffSymbolChange],
    configuration: &[leantoken::DiffConfigurationChange],
) -> BTreeSet<String> {
    let mut returned = symbols.iter().map(symbol_truth).collect::<BTreeSet<_>>();
    returned.extend(configuration.iter().map(|change| {
        format!(
            "config:{}:{}",
            match change.kind {
                DiffConfigurationChangeKind::Added => "added",
                DiffConfigurationChangeKind::Removed => "removed",
                DiffConfigurationChangeKind::Modified => "modified",
            },
            change.key_path
        )
    }));
    returned
}

fn symbol_truth(change: &DiffSymbolChange) -> String {
    let before = change
        .before
        .as_ref()
        .map(|symbol| symbol.name.as_str())
        .unwrap_or("");
    let after = change
        .after
        .as_ref()
        .map(|symbol| symbol.name.as_str())
        .unwrap_or("");
    match change.kind {
        DiffSymbolChangeKind::Added => format!("symbol:added:{after}"),
        DiffSymbolChangeKind::Removed => format!("symbol:removed:{before}"),
        DiffSymbolChangeKind::Renamed => format!("symbol:renamed:{before}->{after}"),
        DiffSymbolChangeKind::Modified => format!(
            "symbol:modified:{after}:{}",
            match change.modification {
                Some(DiffSymbolModification::SignatureChanged) => "signature_changed",
                Some(DiffSymbolModification::BodyOnly) => "body_only",
                None => "unknown",
            }
        ),
    }
}

fn serialized_tokens(
    tokenizer: &Tokenizer,
    response: &ContextResponse,
) -> Result<usize, serde_json::Error> {
    serde_json::to_string(response).map(|json| tokenizer.count(&json))
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn init_git(root: &Path) -> Result<(), Box<dyn Error>> {
    run_git(root, &["init"])?;
    run_git(root, &["config", "user.email", "benchmark@example.com"])?;
    run_git(root, &["config", "user.name", "Benchmark"])?;
    run_git(root, &["add", "."])?;
    run_git(root, &["commit", "-m", "base"])?;
    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

fn required_git_output(root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    git_output(root, args).ok_or_else(|| format!("git {} failed", args.join(" ")).into())
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
