use super::*;

#[tokio::test]
async fn files_enforces_result_limit_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_results;

    services
        .files(files_limit_request(None))
        .await
        .expect("default result limit");
    for requested in [1, limit] {
        services
            .files(files_limit_request(Some(requested)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .files(files_limit_request(Some(0)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .files(files_limit_request(Some(limit + 1)))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", limit + 1, limit);
}
#[tokio::test]
async fn search_enforces_all_limit_contracts() {
    let (_root, services) = fixture().await;
    let result_limit = services.config().max_results;
    let token_limit = services.config().max_output_tokens;

    services
        .search(search_limit_request(None, None, None))
        .await
        .expect("default search limits");
    for requested in [1, result_limit] {
        services
            .search(search_limit_request(Some(requested), Some(1), Some(0)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .search(search_limit_request(Some(0), Some(1), Some(0)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .search(search_limit_request(
            Some(result_limit + 1),
            Some(1),
            Some(0),
        ))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", result_limit + 1, result_limit);

    for requested in [1, token_limit] {
        services
            .search(search_limit_request(Some(1), Some(requested), Some(0)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .search(search_limit_request(Some(1), Some(0), Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .search(search_limit_request(
            Some(1),
            Some(token_limit + 1),
            Some(0),
        ))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", token_limit + 1, token_limit);

    for requested in [0, 1, 20] {
        services
            .search(search_limit_request(Some(1), Some(1), Some(requested)))
            .await
            .expect("valid context-line limit");
    }
    let error = services
        .search(search_limit_request(Some(1), Some(1), Some(21)))
        .await
        .expect_err("oversized context-line limit");
    assert_limit_exceeded(error, "context_lines", 21, 20);
}

#[tokio::test]
async fn outline_enforces_result_and_token_limit_contracts() {
    let (_root, services) = fixture().await;
    let result_limit = services.config().max_results;
    let token_limit = services.config().max_output_tokens;

    services
        .outline(outline_limit_request(None, None))
        .await
        .expect("default outline limits");
    for requested in [1, result_limit] {
        services
            .outline(outline_limit_request(Some(requested), Some(1)))
            .await
            .expect("valid result limit");
    }
    let error = services
        .outline(outline_limit_request(Some(0), Some(1)))
        .await
        .expect_err("zero result limit");
    assert_zero_limit(error, "max_results");
    let error = services
        .outline(outline_limit_request(Some(result_limit + 1), Some(1)))
        .await
        .expect_err("oversized result limit");
    assert_limit_exceeded(error, "max_results", result_limit + 1, result_limit);

    for requested in [1, token_limit] {
        services
            .outline(outline_limit_request(Some(1), Some(requested)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .outline(outline_limit_request(Some(1), Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .outline(outline_limit_request(Some(1), Some(token_limit + 1)))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", token_limit + 1, token_limit);
}

#[tokio::test]
async fn read_enforces_token_limit_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_output_tokens;

    services
        .read(read_limit_request(None))
        .await
        .expect("default token limit");
    for requested in [1, limit] {
        services
            .read(read_limit_request(Some(requested)))
            .await
            .expect("valid token limit");
    }
    let error = services
        .read(read_limit_request(Some(0)))
        .await
        .expect_err("zero token limit");
    assert_zero_limit(error, "max_tokens");
    let error = services
        .read(read_limit_request(Some(limit + 1)))
        .await
        .expect_err("oversized token limit");
    assert_limit_exceeded(error, "max_tokens", limit + 1, limit);
}

#[tokio::test]
async fn context_enforces_token_budget_contract() {
    let (_root, services) = fixture().await;
    let limit = services.config().max_output_tokens;

    for requested in [1, limit] {
        services
            .context(context_limit_request(requested))
            .await
            .expect("valid token budget");
    }
    let error = services
        .context(context_limit_request(0))
        .await
        .expect_err("zero token budget");
    assert_zero_limit(error, "token_budget");
    let error = services
        .context(context_limit_request(limit + 1))
        .await
        .expect_err("oversized token budget");
    assert_limit_exceeded(error, "token_budget", limit + 1, limit);
}

#[tokio::test]
async fn context_tiny_budget_does_not_claim_candidates_are_missing() {
    let (_root, services) = fixture().await;

    let response = services
        .context(context_limit_request(1))
        .await
        .expect("tiny valid token budget");

    assert!(response.fragments.is_empty());
    assert!(response.omission_summary.budget_or_result_limit > 0);
    assert!(
        !response
            .warnings
            .iter()
            .any(|warning| warning == "no relevant indexed evidence found")
    );
}

#[tokio::test]
async fn regex_search_respects_absolute_candidate_cap() {
    let root = tempfile::tempdir().expect("root");
    // Many matching files so limit*20 alone would exceed MAX_REGEX_CANDIDATES if
    // uncapped; the hard cap must still bound results.
    for index in 0..80 {
        std::fs::write(
            root.path().join(format!("f{index}.rs")),
            "fn needle() { let needle = 1; }\n".repeat(40),
        )
        .expect("write");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("refresh");

    let response = services
        .search(SearchRequest {
            query: "needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(32_000),
            context_lines: Some(0),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("regex search");
    assert!(!response.hits.is_empty());
    // max_results bounds the returned page, but the path must complete without
    // scanning unbounded; generation must be a committed snapshot.
    assert!(response.meta.repository_generation >= 1);
    assert!(response.hits.len() <= 100);
}
