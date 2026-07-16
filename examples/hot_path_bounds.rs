//! Reproducible retrieval hot-path measurement for issue #24.
//!
//! Builds a configurable synthetic repository, indexes once, warms both paths,
//! then reports p50/p95 wall time for regex search and context assembly.
//!
//! ```bash
//! cargo run --example hot_path_bounds --release -- --files 10000 --iterations 20
//! ```

use std::time::{Duration, Instant};

use clap::Parser;
use leantoken::{Config, ContextRequest, SearchMode, SearchRequest, services::Services};

#[derive(Debug, Parser)]
#[command(about = "Measure bounded regex and context retrieval paths")]
struct Args {
    /// Number of synthetic Rust source files.
    #[arg(long, default_value_t = 2_000)]
    files: usize,
    /// Timed samples per retrieval path, after one warm-up call.
    #[arg(long, default_value_t = 10)]
    iterations: usize,
    /// Approximate source lines per file.
    #[arg(long, default_value_t = 64)]
    file_lines: usize,
}

#[tokio::main]
async fn main() -> leantoken::Result<()> {
    let args = Args::parse();
    if args.files == 0 || args.iterations == 0 || args.file_lines < 4 {
        return Err(leantoken::Error::InvalidRequest(
            "files and iterations must be positive; file-lines must be at least 4".into(),
        ));
    }

    let root = tempfile::tempdir()?;
    for index in 0..args.files {
        let directory = root.path().join(format!("crate_{:03}", index % 64));
        std::fs::create_dir_all(&directory)?;
        let filler = (0..args.file_lines.saturating_sub(4))
            .map(|line| format!("    let value_{line} = {line};\n"))
            .collect::<String>();
        let body = format!(
            "fn item_{index}() {{\n    let needle = {index};\n{filler}    consume(needle);\n}}\n"
        );
        std::fs::write(directory.join(format!("f{index:05}.rs")), body)?;
    }
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database))?;
    let services = Services::open(config)?;

    let index_started = Instant::now();
    let indexed = services.index(false).await?;
    let index_elapsed = index_started.elapsed();

    let regex_request = SearchRequest {
        // Deliberately absent so regex exercises the configured file-scan
        // boundary instead of returning early after its candidate cap.
        query: r"needle\s*=\s*-1".into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(8_000),
        context_lines: Some(0),
        case_sensitive: false,
        cursor: None,
    };
    let context_request = ContextRequest {
        task: "find needle item helpers".into(),
        token_budget: 1_200,
        focus_paths: Vec::new(),
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        prior_repository_generation: None,
    };

    services.search(regex_request.clone()).await?;
    services.context(context_request.clone()).await?;

    let mut regex_durations = Vec::with_capacity(args.iterations);
    let mut regex_hits = 0usize;
    for _ in 0..args.iterations {
        let started = Instant::now();
        regex_hits = services.search(regex_request.clone()).await?.hits.len();
        regex_durations.push(started.elapsed());
    }

    let mut context_durations = Vec::with_capacity(args.iterations);
    let mut context_fragments = 0usize;
    for _ in 0..args.iterations {
        let started = Instant::now();
        context_fragments = services
            .context(context_request.clone())
            .await?
            .fragments
            .len();
        context_durations.push(started.elapsed());
    }

    let report = serde_json::json!({
        "schema_version": 2,
        "host_os": std::env::consts::OS,
        "host_arch": std::env::consts::ARCH,
        "release_build": !cfg!(debug_assertions),
        "fixture": {
            "files": args.files,
            "approximate_lines_per_file": args.file_lines,
            "iterations": args.iterations,
        },
        "index": {
            "generation": indexed.repository_generation,
            "files_indexed": indexed.files_indexed,
            "elapsed_ms": milliseconds(index_elapsed),
        },
        "regex": {
            "hits": regex_hits,
            "timing_ms": timing_stats(regex_durations),
            "candidate_cap": 2_000,
            "files_scanned_cap": 10_000,
            "chunks_per_file_cap": 256,
        },
        "context": {
            "fragments": context_fragments,
            "timing_ms": timing_stats(context_durations),
            "query_cap": 12,
            "symbol_and_reference_hits_per_query": 20,
            "lexical_hits_per_query": 30,
        },
        "limitations": [
            "Synthetic Rust controls file count and size but not real monorepo language mix or directory shape.",
            "Measurements are warm, host-local wall times; run under /usr/bin/time -v for process CPU and peak RSS.",
            "Compare runs only on the same host and release profile.",
        ],
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn timing_stats(mut durations: Vec<Duration>) -> serde_json::Value {
    durations.sort_unstable();
    let percentile = |numerator: usize| {
        let index = durations
            .len()
            .saturating_mul(numerator)
            .div_ceil(100)
            .saturating_sub(1)
            .min(durations.len().saturating_sub(1));
        milliseconds(durations[index])
    };
    serde_json::json!({
        "samples": durations.len(),
        "min": milliseconds(durations[0]),
        "p50": percentile(50),
        "p95": percentile(95),
        "max": milliseconds(*durations.last().expect("non-empty samples")),
    })
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
