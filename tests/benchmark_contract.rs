use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use leantoken::{Config, ContextRequest, services::Services, tokens};
use serde::Serialize;

#[derive(Clone, Copy)]
struct Task {
    prompt: &'static str,
    relevant: &'static [&'static str],
}

const TASKS: &[Task] = &[
    Task {
        prompt: "change the Rust Point distance calculation and its caller",
        relevant: &["src/rust/math.rs", "src/rust/main.rs"],
    },
    Task {
        prompt: "update the JavaScript greet helper and its caller",
        relevant: &["src/js/utils.js", "src/js/app.js"],
    },
    Task {
        prompt: "change the Python Greeter greet method and its caller",
        relevant: &["src/python/greeter.py", "src/python/main.py"],
    },
    Task {
        prompt: "change the Go Point Distance method and its caller",
        relevant: &["src/go/point.go", "src/go/main.go"],
    },
    Task {
        prompt: "change the TypeScript Box area using Point",
        relevant: &["src/ts/box.ts", "src/ts/point.ts"],
    },
];

#[derive(Serialize)]
struct TaskReport {
    task: &'static str,
    relevant_files: &'static [&'static str],
    returned_files: Vec<String>,
    relevant_files_found: usize,
    recall: f64,
    baseline_full_file_source_tokens: usize,
    baseline_minimal_read_json_tokens: usize,
    leantoken_source_tokens: usize,
    leantoken_total_json_tokens: usize,
    source_token_savings_fraction: f64,
    total_json_savings_against_minimal_baseline_fraction: f64,
    warm_latency_ms: f64,
    second_response_source_tokens: usize,
    estimated_repeated_range_source_tokens: usize,
    known_fragments_resent: usize,
}

#[derive(Serialize)]
struct Report {
    fixture: String,
    tokenizer: &'static str,
    token_count_exact: bool,
    task_count: usize,
    cold_index_ms: f64,
    aggregate_baseline_source_tokens: usize,
    aggregate_leantoken_source_tokens: usize,
    aggregate_source_token_savings_fraction: f64,
    aggregate_baseline_minimal_read_json_tokens: usize,
    aggregate_leantoken_total_json_tokens: usize,
    aggregate_total_json_savings_against_minimal_baseline_fraction: f64,
    aggregate_recall: f64,
    tasks: Vec<TaskReport>,
    limitations: Vec<&'static str>,
}

#[tokio::test]
async fn benchmark_token_economy() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample_repo");
    let temp = tempfile::tempdir().expect("temporary benchmark repository");
    let root = temp.path().join("repo");
    copy_tree(&source, &root);
    let config = Config::discover(&root, Some(temp.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");

    let cold_start = Instant::now();
    services.index(true).await.expect("cold index");
    let cold_index_ms = cold_start.elapsed().as_secs_f64() * 1_000.0;

    let mut task_reports = Vec::new();
    let mut baseline_total = 0usize;
    let mut emitted_total = 0usize;
    let mut baseline_json_total = 0usize;
    let mut leantoken_json_total = 0usize;
    let mut relevant_total = 0usize;
    let mut found_total = 0usize;

    for task in TASKS {
        let request = ContextRequest {
            task: task.prompt.into(),
            token_budget: 500,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        };
        let warm_start = Instant::now();
        let response = services.context(request.clone()).await.expect("context");
        let warm_latency_ms = warm_start.elapsed().as_secs_f64() * 1_000.0;
        let returned = response
            .fragments
            .iter()
            .map(|fragment| fragment.path.clone())
            .collect::<HashSet<_>>();
        let found = task
            .relevant
            .iter()
            .filter(|path| returned.contains(**path))
            .count();
        let baseline_files = task
            .relevant
            .iter()
            .map(|path| {
                let content = fs::read_to_string(root.join(path)).expect("fixture file");
                serde_json::json!({ "path": path, "content": content })
            })
            .collect::<Vec<_>>();
        let baseline = baseline_files
            .iter()
            .map(|file| tokens::count(file["content"].as_str().expect("content string")))
            .sum::<usize>();
        let baseline_json = tokens::count(
            &serde_json::to_string(&baseline_files).expect("baseline read responses"),
        );
        let emitted = response.meta.emitted_tokens;
        let total_json = tokens::count(&serde_json::to_string(&response).expect("response JSON"));
        let known = response
            .fragments
            .iter()
            .map(|fragment| fragment.content_hash.clone())
            .collect::<Vec<_>>();
        let known_set = known.iter().cloned().collect::<HashSet<_>>();
        let repeated = services
            .context(ContextRequest {
                known_hashes: known,
                prior_repository_generation: Some(response.meta.repository_generation),
                ..request
            })
            .await
            .expect("known-hash context");
        let known_fragments_resent = repeated
            .fragments
            .iter()
            .filter(|fragment| known_set.contains(&fragment.content_hash))
            .count();
        let estimated_repeated_range_source_tokens = repeated
            .fragments
            .iter()
            .map(|fragment| {
                let prior_ranges = response
                    .fragments
                    .iter()
                    .filter(|prior| prior.path == fragment.path)
                    .map(|prior| (prior.start_line, prior.end_line))
                    .collect::<Vec<_>>();
                repeated_range_token_estimate(
                    fragment.start_line,
                    fragment.end_line,
                    fragment.token_count,
                    &prior_ranges,
                )
            })
            .sum();
        assert!(
            repeated
                .omitted
                .iter()
                .any(|candidate| candidate.reason == "known hash"),
            "known-hash omission should be visible in the bounded omission summary"
        );

        baseline_total += baseline;
        emitted_total += emitted;
        baseline_json_total += baseline_json;
        leantoken_json_total += total_json;
        relevant_total += task.relevant.len();
        found_total += found;
        let mut returned_files = returned.into_iter().collect::<Vec<_>>();
        returned_files.sort_unstable();
        task_reports.push(TaskReport {
            task: task.prompt,
            relevant_files: task.relevant,
            returned_files,
            relevant_files_found: found,
            recall: ratio(found, task.relevant.len()),
            baseline_full_file_source_tokens: baseline,
            baseline_minimal_read_json_tokens: baseline_json,
            leantoken_source_tokens: emitted,
            leantoken_total_json_tokens: total_json,
            source_token_savings_fraction: savings(baseline, emitted),
            total_json_savings_against_minimal_baseline_fraction: savings(
                baseline_json,
                total_json,
            ),
            warm_latency_ms,
            second_response_source_tokens: repeated.meta.emitted_tokens,
            estimated_repeated_range_source_tokens,
            known_fragments_resent,
        });
    }

    let aggregate_savings = savings(baseline_total, emitted_total);
    let aggregate_json_savings = savings(baseline_json_total, leantoken_json_total);
    let aggregate_recall = ratio(found_total, relevant_total);
    let report = Report {
        fixture: source.display().to_string(),
        tokenizer: tokens::Tokenizer::default().name(),
        token_count_exact: tokens::Tokenizer::default().is_exact(),
        task_count: TASKS.len(),
        cold_index_ms,
        aggregate_baseline_source_tokens: baseline_total,
        aggregate_leantoken_source_tokens: emitted_total,
        aggregate_source_token_savings_fraction: aggregate_savings,
        aggregate_baseline_minimal_read_json_tokens: baseline_json_total,
        aggregate_leantoken_total_json_tokens: leantoken_json_total,
        aggregate_total_json_savings_against_minimal_baseline_fraction: aggregate_json_savings,
        aggregate_recall,
        tasks: task_reports,
        limitations: vec![
            "Pinned synthetic fixture; not a substitute for benchmarks on maintained real repositories.",
            "Baseline assumes an agent perfectly chooses the labeled relevant files, so it excludes grep output and wrong turns.",
            "Baseline JSON is only a minimal path/content envelope, not a matched tool schema; its comparison with the full LeanToken response is a conservative diagnostic, not a product claim.",
            "Source-token savings and total serialized tool-result tokens are reported separately.",
            "Tiny labeled files can make direct oracle reads cheaper than ranked context; negative savings are retained rather than treated as a harness failure.",
            "No model is executed; edit success, plan handoff, and prewalk quality require a separate agent evaluation.",
        ],
    };
    let pretty = serde_json::to_string_pretty(&report).expect("serialize report");
    let report_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/benchmark_contract_report.json");
    fs::write(&report_path, &pretty).expect("write report");
    println!("{pretty}");

    assert!(aggregate_savings.is_finite());
    assert!(
        aggregate_recall >= 0.80,
        "labeled-file recall below MVP target"
    );
    assert!(
        report
            .tasks
            .iter()
            .all(|task| task.known_fragments_resent == 0)
    );
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn savings(baseline: usize, actual: usize) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        1.0 - actual as f64 / baseline as f64
    }
}

fn repeated_range_token_estimate(
    start_line: usize,
    end_line: usize,
    token_count: usize,
    prior_ranges: &[(usize, usize)],
) -> usize {
    if end_line < start_line || token_count == 0 {
        return 0;
    }
    let line_count = end_line - start_line + 1;
    let repeated_lines = (start_line..=end_line)
        .filter(|line| {
            prior_ranges
                .iter()
                .any(|(prior_start, prior_end)| prior_start <= line && line <= prior_end)
        })
        .count();
    token_count
        .saturating_mul(repeated_lines)
        .div_ceil(line_count)
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create destination");
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target: PathBuf = destination.join(entry.file_name());
        if entry.file_type().expect("fixture type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture file");
        }
    }
}
