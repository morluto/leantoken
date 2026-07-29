//! Measure bounded exhaustive-regex fallback work and process memory.
//!
//! The parent process prepares one synthetic repository at the exact full-scan
//! file boundary, indexes it once, and runs each workload in a fresh child
//! process so peak RSS is not inherited from another search shape.

use std::error::Error as StdError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::model::{SearchEvaluation, SearchMode, SearchRequest};
use leantoken::services::Services;
use leantoken::{Config, Result as LeanTokenResult};
use serde::Serialize;
use serde_json::{Value, json};

type AnyResult<T> = Result<T, Box<dyn StdError>>;

const FULL_SCAN_FILE_BOUNDARY: usize = 10_000;
const FULL_SCAN_CHUNK_BOUNDARY: usize = 256;
const DEFAULT_CHUNK_LINES: usize = 80;
const COMMON_MATCH_INTERVAL: usize = 100;

#[derive(Debug, Parser)]
#[command(about = "Profile exhaustive regex fallback parity, work, and peak RSS")]
struct Args {
    /// Number of synthetic files. The default is the production full-scan bound.
    #[arg(long, default_value_t = FULL_SCAN_FILE_BOUNDARY)]
    synthetic_files: usize,
    /// Raw JSON report path. The report is always printed to stdout as well.
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, hide = true)]
    child: bool,
    #[arg(long, hide = true)]
    repository: Option<PathBuf>,
    #[arg(long, hide = true)]
    database: Option<PathBuf>,
    #[arg(long, hide = true)]
    label: Option<String>,
    #[arg(long, hide = true)]
    pattern: Option<String>,
}

#[derive(Debug, Serialize)]
struct OperationReport {
    elapsed_ms: f64,
    baseline_rss_kib: Option<u64>,
    peak_rss_kib: Option<u64>,
    peak_rss_delta_kib: Option<u64>,
    outcome: Value,
    phases: Option<leantoken::SearchPhaseCounters>,
}

#[derive(Debug, Serialize)]
struct WorkloadReport {
    label: String,
    pattern: String,
    exact_result_parity: bool,
    optimized: OperationReport,
    forced_full_scan: OperationReport,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> AnyResult<()> {
    let args = Args::parse();
    if args.child {
        return run_child(args).await;
    }
    run_parent(args).await
}

async fn run_parent(args: Args) -> AnyResult<()> {
    if args.synthetic_files == 0 || args.synthetic_files > FULL_SCAN_FILE_BOUNDARY {
        return Err(
            format!("synthetic-files must be between 1 and {FULL_SCAN_FILE_BOUNDARY}").into(),
        );
    }
    let repository = tempfile::tempdir()?;
    create_synthetic_repository(repository.path(), args.synthetic_files)?;
    let database_dir = tempfile::tempdir()?;
    let database = database_dir.path().join("regex-fallback.sqlite");
    let config = Config::discover(repository.path(), Some(database.clone()))?;
    let services = Services::open(config)?;
    let index = services.index(true).await?;
    drop(services);

    let workloads = [
        ("sparse_positive", "sparse_marker_boundary"),
        ("common_positive", "common_marker"),
        ("file_boundary_negative", "absent_boundary_marker"),
    ];
    let current_executable = std::env::current_exe()?;
    let mut reports = Vec::with_capacity(workloads.len());
    for (label, pattern) in workloads {
        let output = Command::new(&current_executable)
            .arg("--child")
            .arg("--repository")
            .arg(repository.path())
            .arg("--database")
            .arg(&database)
            .arg("--label")
            .arg(label)
            .arg("--pattern")
            .arg(pattern)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "{label} child failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        reports.push(serde_json::from_slice::<Value>(&output.stdout)?);
    }

    let report = json!({
        "schema_version": 1,
        "release_build": !cfg!(debug_assertions),
        "source_tree": source_tree_provenance(),
        "host": host_provenance(),
        "corpus": {
            "kind": "generated_boundary_fixture",
            "files": args.synthetic_files,
            "source_bytes": synthetic_source_bytes(args.synthetic_files),
            "full_scan_file_boundary": FULL_SCAN_FILE_BOUNDARY,
            "full_scan_chunk_boundary": FULL_SCAN_CHUNK_BOUNDARY,
            "common_match_interval": COMMON_MATCH_INTERVAL,
            "indexed_files": index.files_indexed,
            "repository_generation": index.repository_generation,
        },
        "methodology": {
            "profile": "release",
            "workload_processes": "one fresh child process per shape",
            "peak_rss": "1 ms VmRSS sampling plus child VmHWM on Linux",
            "parity": "complete SearchResponse equality after removing opaque receipt_id and the three accounting fields derived from its serialized value",
            "fallback_selection": "case-insensitive regex forces the bounded full-scan path",
            "limitations": [
                "The generated corpus proves boundary mechanics and memory accounting, not performance on every real repository.",
                "Peak RSS includes process startup, SQLite connection pools, and the complete search response.",
                "The common-positive workload retains one matching chunk per 100 files so the final exact response remains inside the product output bound."
            ],
        },
        "workloads": reports,
    });
    let pretty = serde_json::to_string_pretty(&report)?;
    if let Some(path) = args.output {
        create_output_parent(&path)?;
        fs::write(path, &pretty)?;
    }
    println!("{pretty}");
    Ok(())
}

async fn run_child(args: Args) -> AnyResult<()> {
    let repository = args.repository.ok_or("child repository is required")?;
    let database = args.database.ok_or("child database is required")?;
    let label = args.label.ok_or("child label is required")?;
    let pattern = args.pattern.ok_or("child pattern is required")?;
    let config = Config::discover(&repository, Some(database))?;
    let services = Services::open(config)?;
    let request = SearchRequest {
        query: pattern.clone(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(32_000),
        context_lines: Some(0),
        case_sensitive: false,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };

    let (optimized, optimized_report) =
        measure_search(services.search_evaluation(request.clone())).await;
    let (full_scan, full_scan_report) =
        measure_search(services.search_full_scan_evaluation(request)).await;
    let exact_result_parity = canonical_outcome(&optimized) == canonical_outcome(&full_scan);
    let report = WorkloadReport {
        label,
        pattern,
        exact_result_parity,
        optimized: optimized_report,
        forced_full_scan: full_scan_report,
    };
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

async fn measure_search(
    operation: impl std::future::Future<Output = LeanTokenResult<SearchEvaluation>>,
) -> (LeanTokenResult<SearchEvaluation>, OperationReport) {
    let baseline_rss_kib = process_status_kib("VmRSS:");
    let stop = Arc::new(AtomicBool::new(false));
    let sampler_stop = Arc::clone(&stop);
    let sampler = tokio::spawn(async move {
        let mut peak = process_status_kib("VmRSS:");
        while !sampler_stop.load(Ordering::Acquire) {
            if let Some(current) = process_status_kib("VmRSS:") {
                peak = Some(peak.unwrap_or(0).max(current));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if let Some(high_water) = process_status_kib("VmHWM:") {
            peak = Some(peak.unwrap_or(0).max(high_water));
        }
        peak
    });
    let started = Instant::now();
    let result = operation.await;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    stop.store(true, Ordering::Release);
    let peak_rss_kib = sampler.await.ok().flatten();
    let peak_rss_delta_kib = baseline_rss_kib
        .zip(peak_rss_kib)
        .map(|(baseline, peak)| peak.saturating_sub(baseline));
    let outcome = search_outcome(&result);
    let phases = result
        .as_ref()
        .ok()
        .map(|evaluation| evaluation.phases.clone());
    (
        result,
        OperationReport {
            elapsed_ms,
            baseline_rss_kib,
            peak_rss_kib,
            peak_rss_delta_kib,
            outcome,
            phases,
        },
    )
}

fn create_synthetic_repository(root: &Path, files: usize) -> AnyResult<()> {
    for index in 0..files {
        let extension = if index + 1 == files { "txt" } else { "rs" };
        fs::write(
            root.join(format!("file_{index:05}.{extension}")),
            synthetic_source(index, files),
        )?;
    }
    Ok(())
}

fn create_output_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn synthetic_source_bytes(files: usize) -> u64 {
    (0..files)
        .map(|index| u64::try_from(synthetic_source(index, files).len()).unwrap_or(u64::MAX))
        .sum()
}

fn synthetic_source(index: usize, files: usize) -> String {
    let mut source = format!("pub const filler_{index:05}: usize = {index};\n");
    if index.is_multiple_of(COMMON_MATCH_INTERVAL) {
        source.push_str(&format!(
            "pub const common_marker_{index:05}: bool = true;\n"
        ));
    }
    if index + 1 == files {
        source.push_str("pub const sparse_marker_boundary: bool = true;\n");
        let target_lines = FULL_SCAN_CHUNK_BOUNDARY * DEFAULT_CHUNK_LINES;
        let present_lines = source.lines().count();
        for _ in present_lines..target_lines {
            source.push_str("boundary padding\n");
        }
    }
    source
}

fn canonical_outcome(result: &LeanTokenResult<SearchEvaluation>) -> Value {
    match result {
        Ok(evaluation) => {
            let mut response = serde_json::to_value(&evaluation.response)
                .expect("SearchResponse serialization is infallible");
            if let Some(meta) = response.get_mut("meta").and_then(Value::as_object_mut) {
                meta.insert("receipt_id".into(), Value::Null);
                for field in [
                    "path_and_metadata_tokens",
                    "total_response_tokens",
                    "total_response_tokens",
                ] {
                    meta.insert(field.into(), Value::from(0));
                }
            }
            json!({"status": "ok", "response": response})
        }
        Err(error) => json!({"status": "error", "error": error.to_string()}),
    }
}

fn search_outcome(result: &LeanTokenResult<SearchEvaluation>) -> Value {
    match result {
        Ok(evaluation) => json!({
            "status": "ok",
            "hits": evaluation.response.hits.len(),
            "occurrences_returned": evaluation.response.occurrences_returned,
            "occurrences_total": evaluation.response.occurrences_total,
            "response_tokens": evaluation.response.meta.total_response_tokens,
        }),
        Err(error) => json!({"status": "error", "error": error.to_string()}),
    }
}

fn process_status_kib(field: &str) -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with(field))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
}

fn source_tree_provenance() -> Value {
    let revision = command_output("git", &["rev-parse", "HEAD"]);
    let status = command_output("git", &["status", "--porcelain=v1"]);
    json!({
        "revision": revision,
        "dirty": status.as_ref().is_none_or(|value| !value.is_empty()),
        "untracked_audit_reports_present": status.as_ref().is_some_and(|value| {
            value.lines().any(|line| line.contains("test_suite_audit"))
                || value.lines().any(|line| line.contains("test_suite_deep_audit"))
        }),
    })
}

fn host_provenance() -> Value {
    let memory_kib = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find(|line| line.starts_with("MemTotal:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u64>().ok())
        });
    json!({
        "os": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "rustc": command_output("rustc", &["--version"]),
        "logical_cpus": std::thread::available_parallelism().ok().map(usize::from),
        "memory_kib": memory_kib,
    })
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_fixture_has_sparse_common_and_boundary_shapes() {
        let root = tempfile::tempdir().expect("root");
        create_synthetic_repository(root.path(), 201).expect("fixture");
        let sources = fs::read_dir(root.path())
            .expect("read fixture")
            .map(|entry| fs::read_to_string(entry.expect("entry").path()).expect("source"))
            .collect::<Vec<_>>();
        assert_eq!(sources.len(), 201);
        assert_eq!(
            sources
                .iter()
                .filter(|source| source.contains("common_marker"))
                .count(),
            3
        );
        assert_eq!(
            sources
                .iter()
                .filter(|source| source.contains("sparse_marker_boundary"))
                .count(),
            1
        );
        assert!(
            sources
                .iter()
                .all(|source| !source.contains("absent_boundary_marker"))
        );
        assert_eq!(
            sources.iter().map(|source| source.lines().count()).max(),
            Some(FULL_SCAN_CHUNK_BOUNDARY * DEFAULT_CHUNK_LINES)
        );
    }

    #[test]
    fn bare_output_filename_uses_current_directory() {
        create_output_parent(Path::new("profile.json")).expect("bare output path");
    }
}
