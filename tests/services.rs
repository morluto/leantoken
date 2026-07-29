use std::time::Instant;

use leantoken::{
    Config, ContextRequest, ContextRequiredEvidence, ContextResponseProfile, ContextSignalPolicy,
    ContextWorkflow, DiffSymbolsIncompleteReason, DiffSymbolsRequest, DiffSymbolsStatus,
    DiffSymbolsTarget, Error, FileOperation, FilesRequest, Freshness, HandoffManifestRequest,
    HandoffValidation, HandoffValidationStatus, HandoffWorkingTreeState, HistoryOperation,
    HistoryRequest, IndexConsistency, IndexState, JsonIncompleteReason, JsonOperation,
    JsonProjection, JsonRequest, JsonSelector, OutlinePathResult, OutlinePathStatus, OutlineRequest,
    ReadDeltaBaseSource, ReadDeltaFallback, ReadDeltaOutcome, ReadDeltaPersistenceFallback,
    ReadRequest, ReadStatus, ReferenceRole, SearchMode, SearchRequest, TokenAccountingOperation,
    TokenSavingsOperation, TokenSavingsWindow, WorkflowEvidence,
    coordination::IndexCoordination,
    services::{ServiceCallOptions, Services},
    tokens::Tokenizer,
};
use leantoken::{
    DiffConfigurationChangeKind, DiffOwnerTestStatus, DiffSymbolChangeKind, DiffSymbolModification,
};
use tokio_util::sync::CancellationToken;

macro_rules! assert_response_token_accounting {
    ($response:expr, $tokenizer:expr) => {{
        let response = &$response;
        let tokenizer = $tokenizer;
        assert_eq!(response.meta.source_tokens, response.meta.emitted_tokens);
        assert_eq!(response.meta.tokenizer, tokenizer.name());
        assert_eq!(response.meta.token_count_exact, tokenizer.is_exact());
        assert!(response.meta.protocol_tokens > 0);
        assert_eq!(
            response.meta.total_response_tokens,
            response.meta.source_tokens
                + response.meta.protocol_tokens
                + response.meta.path_and_metadata_tokens
        );
        assert_eq!(
            response.meta.payload_tokens,
            response.meta.total_response_tokens
        );
        assert!(response.meta.payload_tokens > 0);

        let final_payload =
            serde_json::to_string(response).expect("serialize final response payload");
        assert_eq!(
            tokenizer.count(&final_payload),
            response.meta.total_response_tokens,
            "accounting must include its own serialized fields"
        );
    }};
}

async fn fixture() -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("create src");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn greet(name: &str) -> String {\n    format!(\"hello {name}\")\n}\n\npub fn caller() {\n    let _ = greet(\"agent\");\n}\n",
    )
    .expect("write fixture");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    (root, services)
}

mod budgets;
mod receipts;
mod search_planning;
mod path_safety;
mod context_workflow;
mod context_planning;
mod limits;
mod repository;
mod smoke;
mod savings;
mod languages;
mod context_signals;
mod files;
mod search;
mod read;
mod outline;
mod lifecycle;
mod history;
mod json;
mod consistency;
mod diff;

fn assert_zero_limit(error: Error, expected_field: &'static str) {
    assert!(
        matches!(
            error,
            Error::InvalidInput {
                field,
                reason: "must be greater than zero"
            } if field == expected_field
        ),
        "unexpected zero-limit error: {error:?}"
    );
}

fn assert_limit_exceeded(
    error: Error,
    expected_field: &'static str,
    expected_requested: usize,
    expected_limit: usize,
) {
    assert!(
        matches!(
            error,
            Error::RequestLimitExceeded {
                field,
                requested,
                limit,
            } if field == expected_field
                && requested == expected_requested
                && limit == expected_limit
        ),
        "unexpected request-limit error: {error:?}"
    );
}

fn files_limit_request(max_results: Option<usize>) -> FilesRequest {
    FilesRequest {
        operation: FileOperation::Tree,
        path: None,
        query: None,
        pattern: None,
        max_results,
        cursor: None,
        depth: Some(0),
    }
}

fn search_limit_request(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
    context_lines: Option<usize>,
) -> SearchRequest {
    SearchRequest {
        query: "greet".into(),
        mode: SearchMode::Text,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results,
        max_tokens,
        context_lines,
        case_sensitive: false,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        cursor: None,
    }
}

fn outline_limit_request(
    max_results: Option<usize>,
    max_tokens: Option<usize>,
) -> OutlineRequest {
    OutlineRequest {
        paths: vec!["src/lib.rs".into()],
        symbol_name: None,
        symbol_kind: None,
        max_results,
        max_tokens,
        receipt_id: None,
        cursor: None,
    }
}

fn read_limit_request(max_tokens: Option<usize>) -> ReadRequest {
    ReadRequest {
        path: "src/lib.rs".into(),
        start_line: Some(1),
        end_line: Some(1),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens,
        expected_hash: None,
        delta: false,
        receipt_id: None,
    }
}

fn context_limit_request(token_budget: usize) -> ContextRequest {
    ContextRequest {
        task: "find greet".into(),
        token_budget,
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
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        verbose_diagnostics: false,
    }
}

async fn tree_pages(
    services: &Services,
    path: Option<&str>,
) -> Vec<(serde_json::Value, Option<String>)> {
    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let response = services
            .files(FilesRequest {
                operation: FileOperation::Tree,
                path: path.map(str::to_owned),
                query: None,
                pattern: None,
                max_results: Some(2),
                cursor,
                depth: Some(2),
            })
            .await
            .expect("tree page");
        let next = response.meta.next_cursor;
        pages.push((
            serde_json::to_value(response.entries).expect("serialize tree entries"),
            next.clone(),
        ));
        let Some(next) = next else {
            break;
        };
        cursor = Some(next);
    }
    pages
}

async fn indexed_source(path: &str, content: &[u8]) -> (tempfile::TempDir, Services) {
    let root = tempfile::tempdir().expect("temporary repository");
    let source_path = root.path().join(path);
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent).expect("create source parent");
    }
    std::fs::write(source_path, content).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index source");
    (root, services)
}

fn require_git() {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .expect("git is required to run git-dependent integration tests");
    assert!(
        output.status.success(),
        "git is required to run git-dependent integration tests: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo(root: &std::path::Path) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command");
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}
