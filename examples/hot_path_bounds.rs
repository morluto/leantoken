//! Reproducible micro-measurement for retrieval hot-path bounds (#24).
//!
//! Builds a synthetic repository, indexes once, then times regex search and
//! context assembly. Prints JSON with wall-clock ms and result sizes so caps
//! can be checked without monorepo speculation. Numbers are host-local only.
//!
//! ```bash
//! cargo run --example hot_path_bounds --release
//! ```

use std::time::Instant;

use leantoken::{Config, ContextRequest, SearchMode, SearchRequest, services::Services};

#[tokio::main]
async fn main() -> leantoken::Result<()> {
    let root = tempfile::tempdir().expect("root");
    let file_count = 200usize;
    for index in 0..file_count {
        let body =
            format!("fn item_{index}() {{\n    let needle = {index};\n    let _ = needle;\n}}\n");
        std::fs::write(root.path().join(format!("f{index:03}.rs")), body).expect("write");
    }
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database))?;
    let services = Services::open(config)?;

    let index_started = Instant::now();
    let indexed = services.index(false).await?;
    let index_ms = index_started.elapsed().as_secs_f64() * 1_000.0;

    let regex_started = Instant::now();
    let regex = services
        .search(SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(8_000),
            context_lines: Some(0),
            case_sensitive: false,
            cursor: None,
        })
        .await?;
    let regex_ms = regex_started.elapsed().as_secs_f64() * 1_000.0;

    let context_started = Instant::now();
    let context = services
        .context(ContextRequest {
            task: "find needle item helpers".into(),
            token_budget: 1_200,
            focus_paths: Vec::new(),
            focus_symbols: Vec::new(),
            exclude_paths: Vec::new(),
            known_hashes: Vec::new(),
            prior_repository_generation: None,
        })
        .await?;
    let context_ms = context_started.elapsed().as_secs_f64() * 1_000.0;

    let report = serde_json::json!({
        "fixture_files": file_count,
        "repository_generation": indexed.repository_generation,
        "files_indexed": indexed.files_indexed,
        "index_ms": index_ms,
        "regex": {
            "hits": regex.hits.len(),
            "emitted_tokens": regex.meta.emitted_tokens,
            "generation": regex.meta.repository_generation,
            "ms": regex_ms,
            "max_results_requested": 100,
            "hard_candidate_cap": 2000,
            "hard_files_scanned_cap": 10_000,
        },
        "context": {
            "fragments": context.fragments.len(),
            "emitted_tokens": context.meta.emitted_tokens,
            "generation": context.meta.repository_generation,
            "ms": context_ms,
            "max_context_queries": 12,
            "max_hits_per_source": 20,
            "max_lexical_hits": 30,
        },
        "notes": [
            "Host-local wall times only; not a monorepo claim.",
            "Regex returns at most max_results after a hard candidate/file/chunk scan cap.",
            "Context fan-out is term-bounded; full file-list materialization remains O(files).",
        ],
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
