use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use leantoken::{
    Config, ContextRequest, FileOperation, FilesRequest, OutlineRequest, ReadRequest, SearchMode,
    SearchRequest, services::Services, tokens,
};
use serde::Serialize;

const TOKEN_BUDGET: usize = 500;

struct Task {
    prompt: &'static str,
    search_query: &'static str,
    relevant: &'static [&'static str],
}

const TASKS: &[Task] = &[
    Task {
        prompt: "change the Rust Point distance calculation and its caller",
        search_query: "distance",
        relevant: &["src/rust/math.rs", "src/rust/main.rs"],
    },
    Task {
        prompt: "update the JavaScript greet helper and its caller",
        search_query: "greet",
        relevant: &["src/js/utils.js", "src/js/app.js"],
    },
    Task {
        prompt: "change the Python Greeter greet method and its caller",
        search_query: "greet",
        relevant: &["src/python/greeter.py", "src/python/main.py"],
    },
    Task {
        prompt: "change the Go Point Distance method and its caller",
        search_query: "Distance",
        relevant: &["src/go/point.go", "src/go/main.go"],
    },
    Task {
        prompt: "change the TypeScript Box area using Point",
        search_query: "area",
        relevant: &["src/ts/box.ts", "src/ts/point.ts"],
    },
];

#[derive(Serialize)]
struct RepresentationBreakdown {
    source: usize,
    symbol: usize,
    import_neighbor: usize,
    other: usize,
}

#[derive(Serialize)]
struct TaskReport {
    task: &'static str,
    context_source_tokens: usize,
    context_total_json_tokens: usize,
    context_representation_breakdown: RepresentationBreakdown,
    context_full_read_source_tokens: usize,
    context_full_read_total_json_tokens: usize,
    search_source_tokens: usize,
    search_total_json_tokens: usize,
    outline_source_tokens: usize,
    outline_total_json_tokens: usize,
    read_source_tokens: usize,
    read_total_json_tokens: usize,
    repo_map_total_json_tokens: usize,
    context_source_savings_vs_read_fraction: f64,
    context_source_savings_vs_full_read_fraction: f64,
    outline_source_savings_vs_read_fraction: f64,
    search_source_savings_vs_read_fraction: f64,
    latency_ms: f64,
}

#[derive(Default, Serialize)]
struct Aggregate {
    task_count: usize,
    context_source_tokens: usize,
    search_source_tokens: usize,
    outline_source_tokens: usize,
    read_source_tokens: usize,
    context_total_json_tokens: usize,
    context_full_read_source_tokens: usize,
    context_full_read_total_json_tokens: usize,
    search_total_json_tokens: usize,
    outline_total_json_tokens: usize,
    read_total_json_tokens: usize,
    repo_map_total_json_tokens: usize,
    aggregate_context_source_savings_vs_read_fraction: f64,
    aggregate_context_source_savings_vs_full_read_fraction: f64,
    aggregate_outline_source_savings_vs_read_fraction: f64,
    aggregate_search_source_savings_vs_read_fraction: f64,
}

#[derive(Serialize)]
struct Report {
    fixture: String,
    tokenizer: &'static str,
    token_count_exact: bool,
    task_count: usize,
    aggregate: Aggregate,
    tasks: Vec<TaskReport>,
    limitations: Vec<&'static str>,
}

#[tokio::test]
async fn compare_context_representations() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample_repo");
    let temp = tempfile::tempdir().expect("temporary benchmark repository");
    let root = temp.path().join("repo");
    copy_tree(&source, &root);
    let config = Config::discover(&root, Some(temp.path().join("index.sqlite"))).expect("config");
    let tokenizer = config.tokenizer;
    let services = Services::open(config).expect("services");
    services.index(true).await.expect("cold index");

    let repo_map = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(1_000),
            cursor: None,
            depth: Some(2),
        })
        .await
        .expect("repo map");

    let mut task_reports = Vec::new();
    let mut aggregate = Aggregate {
        task_count: TASKS.len(),
        ..Aggregate::default()
    };

    for task in TASKS {
        let started = Instant::now();
        let context_request = ContextRequest {
            task: task.prompt.into(),
            token_budget: TOKEN_BUDGET,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        };
        let context = services
            .context(context_request.clone())
            .await
            .expect("context");

        let context_total_json =
            tokens::count(&serde_json::to_string(&context).expect("context json"));
        let mut context_source = 0usize;
        let mut breakdown = RepresentationBreakdown {
            source: 0,
            symbol: 0,
            import_neighbor: 0,
            other: 0,
        };
        for fragment in &context.fragments {
            context_source += fragment.token_count;
            match fragment.representation.as_str() {
                "source" => breakdown.source += fragment.token_count,
                "symbol" => breakdown.symbol += fragment.token_count,
                "import_neighbor" => breakdown.import_neighbor += fragment.token_count,
                _ => breakdown.other += fragment.token_count,
            }
        }

        let context_paths: HashSet<String> =
            context.fragments.iter().map(|f| f.path.clone()).collect();
        let mut context_full_read_hits = Vec::new();
        for path in &context_paths {
            let response = services
                .read(ReadRequest {
                    path: path.clone(),
                    start_line: None,
                    end_line: None,
                    symbol: None,
                    max_tokens: Some(TOKEN_BUDGET),
                    expected_hash: None,
                })
                .await
                .expect("read context paths");
            context_full_read_hits.push(response);
        }
        let context_full_read_total_json_tokens = tokens::count(
            &serde_json::to_string(&context_full_read_hits).expect("context full read json"),
        );
        let context_full_read_source_tokens = context_full_read_hits
            .iter()
            .map(|r| r.meta.emitted_tokens)
            .sum();

        let search = services
            .search(SearchRequest {
                query: task.search_query.into(),
                mode: SearchMode::Auto,
                include_paths: task.relevant.iter().map(|&p| p.into()).collect(),
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(20),
                max_tokens: Some(TOKEN_BUDGET),
                context_lines: Some(2),
                case_sensitive: false,
                cursor: None,
            })
            .await
            .expect("search");
        let search_total_json =
            tokens::count(&serde_json::to_string(&search).expect("search json"));

        let outline = services
            .outline(OutlineRequest {
                paths: task.relevant.iter().map(|&p| p.into()).collect(),
                symbol_name: None,
                symbol_kind: None,
                max_results: Some(1_000),
                max_tokens: Some(TOKEN_BUDGET),
            })
            .await
            .expect("outline");
        let outline_total_json =
            tokens::count(&serde_json::to_string(&outline).expect("outline json"));

        let mut read_hits = Vec::new();
        for path in task.relevant {
            let response = services
                .read(ReadRequest {
                    path: (*path).into(),
                    start_line: None,
                    end_line: None,
                    symbol: None,
                    max_tokens: Some(TOKEN_BUDGET),
                    expected_hash: None,
                })
                .await
                .expect("read");
            read_hits.push(response);
        }
        let read_total_json_tokens =
            tokens::count(&serde_json::to_string(&read_hits).expect("read json"));
        let read_source_tokens = read_hits.iter().map(|r| r.meta.emitted_tokens).sum();

        let repo_map_total_json =
            tokens::count(&serde_json::to_string(&repo_map).expect("repo map json"));

        let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;

        let report = TaskReport {
            task: task.prompt,
            context_source_tokens: context_source,
            context_total_json_tokens: context_total_json,
            context_representation_breakdown: breakdown,
            context_full_read_source_tokens,
            context_full_read_total_json_tokens,
            search_source_tokens: search.meta.emitted_tokens,
            search_total_json_tokens: search_total_json,
            outline_source_tokens: outline.meta.emitted_tokens,
            outline_total_json_tokens: outline_total_json,
            read_source_tokens,
            read_total_json_tokens,
            repo_map_total_json_tokens: repo_map_total_json,
            context_source_savings_vs_read_fraction: savings(read_source_tokens, context_source),
            context_source_savings_vs_full_read_fraction: savings(
                context_full_read_source_tokens,
                context_source,
            ),
            outline_source_savings_vs_read_fraction: savings(
                read_source_tokens,
                outline.meta.emitted_tokens,
            ),
            search_source_savings_vs_read_fraction: savings(
                read_source_tokens,
                search.meta.emitted_tokens,
            ),
            latency_ms,
        };

        aggregate.context_source_tokens += report.context_source_tokens;
        aggregate.search_source_tokens += report.search_source_tokens;
        aggregate.outline_source_tokens += report.outline_source_tokens;
        aggregate.read_source_tokens += report.read_source_tokens;
        aggregate.context_total_json_tokens += report.context_total_json_tokens;
        aggregate.context_full_read_source_tokens += report.context_full_read_source_tokens;
        aggregate.context_full_read_total_json_tokens += report.context_full_read_total_json_tokens;
        aggregate.search_total_json_tokens += report.search_total_json_tokens;
        aggregate.outline_total_json_tokens += report.outline_total_json_tokens;
        aggregate.read_total_json_tokens += report.read_total_json_tokens;
        aggregate.repo_map_total_json_tokens += report.repo_map_total_json_tokens;

        assert!(
            report.context_source_tokens <= report.context_full_read_source_tokens,
            "context returned more source tokens than a full read of the files it actually returned"
        );
        assert!(
            report.outline_source_tokens <= report.read_source_tokens,
            "outline returned more source tokens than a full read of the labeled files"
        );
        assert!(
            report.search_source_tokens <= TOKEN_BUDGET,
            "search exceeded the explicit token budget"
        );

        task_reports.push(report);
    }

    aggregate.aggregate_context_source_savings_vs_read_fraction = savings(
        aggregate.read_source_tokens,
        aggregate.context_source_tokens,
    );
    aggregate.aggregate_context_source_savings_vs_full_read_fraction = savings(
        aggregate.context_full_read_source_tokens,
        aggregate.context_source_tokens,
    );
    aggregate.aggregate_outline_source_savings_vs_read_fraction = savings(
        aggregate.read_source_tokens,
        aggregate.outline_source_tokens,
    );
    aggregate.aggregate_search_source_savings_vs_read_fraction =
        savings(aggregate.read_source_tokens, aggregate.search_source_tokens);

    let report = Report {
        fixture: source.display().to_string(),
        tokenizer: tokenizer.name(),
        token_count_exact: tokenizer.is_exact(),
        task_count: TASKS.len(),
        aggregate,
        tasks: task_reports,
        limitations: vec![
            "Pinned synthetic fixture; the labeled files are small and real repositories are larger.",
            "Full-file reads, search include-paths, and outline inputs use labeled paths; context receives only the task text.",
            "Source-token savings compare content tokens only; total JSON includes schemas, metadata, and JSON syntax.",
            "Repo-map JSON measures one compact tree listing; it is not a substitute for content.",
            "No model executes an edit, so sufficiency is not measured here.",
        ],
    };

    let pretty = serde_json::to_string_pretty(&report).expect("serialize report");
    let report_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/representation_comparison_report.json");
    fs::write(&report_path, &pretty).expect("write report");
    println!("{pretty}");

    assert!(
        report
            .aggregate
            .aggregate_context_source_savings_vs_read_fraction
            .is_finite()
    );
}

fn savings(baseline: usize, actual: usize) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        1.0 - actual as f64 / baseline as f64
    }
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
