use super::*;

#[tokio::test]
async fn search_applies_path_filters_before_candidate_limits() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "pub fn shared_target() {\n    let shared_lexical_needle = 1;\n}\npub fn caller() { shared_target(); }\n";
    for index in 0..10 {
        std::fs::write(root.path().join(format!("a{index:02}.rs")), source)
            .expect("write excluded source");
    }
    std::fs::write(root.path().join("z_included.rs"), source).expect("write included source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    for (mode, query) in [
        (SearchMode::Symbol, "shared_target"),
        (SearchMode::Reference, "shared_target"),
        (SearchMode::Text, "shared_lexical_needle"),
    ] {
        let response = services
            .search(SearchRequest {
                query: query.into(),
                mode,
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                include_paths: vec!["z_included.rs".into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(200),
                context_lines: Some(0),
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            })
            .await
            .expect("filtered search");
        assert_eq!(response.hits.len(), 1, "{mode:?}");
        assert_eq!(response.hits[0].path, "z_included.rs", "{mode:?}");

        let response = services
            .search(SearchRequest {
                query: query.into(),
                mode,
                case_sensitive: true,
                all_occurrences: false,
                prefer_structural: false,
                include_paths: Vec::new(),
                exclude_paths: vec!["a*.rs".into()],
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(200),
                context_lines: Some(0),
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            })
            .await
            .expect("exclusion-filtered search");
        assert_eq!(response.hits.len(), 1, "{mode:?}");
        assert_eq!(response.hits[0].path, "z_included.rs", "{mode:?}");
    }
}

#[tokio::test]
async fn exhaustive_text_search_returns_each_occurrence_with_exact_total_and_pagination() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "const first = \"audit_key audit_key\";\nconst second = \"audit_key\";\n";
    std::fs::write(root.path().join("occurrences.js"), source).expect("write source");
    std::fs::write(
        root.path().join("excluded.js"),
        "const value = 'audit_key';\n",
    )
    .expect("write excluded source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let request = SearchRequest {
        query: "audit_key".into(),
        mode: SearchMode::Text,
        include_paths: vec!["occurrences.js".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(2),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };

    let first = services
        .search(request.clone())
        .await
        .expect("first occurrence page");

    assert_eq!(first.occurrences_total, Some(3));
    assert_eq!(first.occurrences_returned, 2);
    assert_eq!(first.hits.len(), 2);
    let expected_offsets = source
        .match_indices("audit_key")
        .map(|(start, matched)| (start, start + matched.len()))
        .collect::<Vec<_>>();
    let first_offsets = first
        .hits
        .iter()
        .map(|hit| {
            let occurrence = hit.occurrence.as_ref().expect("exact occurrence");
            (occurrence.start_byte, occurrence.end_byte)
        })
        .collect::<Vec<_>>();
    assert_eq!(first_offsets, expected_offsets[..2]);

    let mut token_limited = request.clone();
    token_limited.max_tokens = Some(1);
    let limited = services
        .search(token_limited)
        .await
        .expect("token-limited occurrence page");
    assert_eq!(limited.occurrences_total, Some(3));
    assert_eq!(limited.occurrences_returned, 0);
    assert!(limited.hits.is_empty());
    assert!(limited.meta.next_cursor.is_some());

    let mut next = request;
    next.cursor = first.meta.next_cursor;
    let second = services.search(next).await.expect("second occurrence page");

    assert_eq!(second.occurrences_total, Some(3));
    assert_eq!(second.occurrences_returned, 1);
    assert!(second.meta.next_cursor.is_none());
    let occurrence = second.hits[0]
        .occurrence
        .as_ref()
        .expect("exact occurrence");
    assert_eq!(
        (occurrence.start_byte, occurrence.end_byte),
        expected_offsets[2]
    );
    assert_eq!(occurrence.start_line, 2);
    assert_eq!(occurrence.end_line, 2);
    assert_eq!(occurrence.start_column, 16);
    assert_eq!(occurrence.end_column, 25);

    let mut short_query = search_limit_request(Some(10), Some(1_000), Some(0));
    short_query.query = "it".into();
    short_query.mode = SearchMode::Text;
    short_query.include_paths = vec!["occurrences.js".into()];
    short_query.case_sensitive = true;
    short_query.all_occurrences = true;
    let short = services
        .search(short_query)
        .await
        .expect("short substring occurrence search");
    assert_eq!(short.occurrences_total, Some(3));
    assert_eq!(short.occurrences_returned, 3);
}

#[tokio::test]
async fn exhaustive_occurrence_groups_preserve_probe_e_coordinates_without_repeated_excerpts() {
    let root = tempfile::tempdir().expect("temporary repository");
    let line = "F4-P 0-RTT forbidden-phase early-data Handshake handshake completion\n";
    let source = line.repeat(10);
    std::fs::write(root.path().join("probe-e.tex"), &source).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let request = SearchRequest {
        query: "F4-P|0-RTT|forbidden-phase|early-data|Handshake|handshake completion".into(),
        mode: SearchMode::Regex,
        include_paths: vec!["probe-e.tex".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(100),
        max_tokens: Some(6_000),
        context_lines: Some(1),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };

    let full = services
        .search(request.clone())
        .await
        .expect("legacy exhaustive response");
    assert_eq!(full.occurrences_total, Some(60));
    assert_eq!(full.occurrences_returned, 60);
    assert_eq!(full.hits.len(), 60);

    let grouped = services
        .search_occurrences(request.clone(), false)
        .await
        .expect("grouped occurrence response");
    assert_eq!(grouped.occurrences_total, 60);
    assert_eq!(grouped.occurrences_returned, 60);
    assert_eq!(grouped.groups_returned, 10);
    assert!(grouped.groups.iter().all(|group| {
        group.excerpt.is_some() && group.content_hash.is_some() && group.occurrences.len() == 6
    }));
    let coordinates = grouped
        .groups
        .iter()
        .flat_map(|group| &group.occurrences)
        .collect::<Vec<_>>();
    assert_eq!(coordinates.len(), 60);
    assert!(coordinates.iter().all(|coordinate| {
        coordinate.end_line.is_none()
            && coordinate.start_column < coordinate.end_column
            && (1..=10).contains(&coordinate.line)
    }));
    assert!(
        grouped.meta.total_response_tokens < 10_941,
        "Probe E grouped response used {} tokens",
        grouped.meta.total_response_tokens
    );
    assert!(
        grouped.meta.total_response_tokens * 2 < full.meta.total_response_tokens,
        "grouped={} full={}",
        grouped.meta.total_response_tokens,
        full.meta.total_response_tokens
    );
    assert_response_token_accounting!(grouped, Tokenizer::default());

    let response_limit = grouped.meta.total_response_tokens.saturating_sub(1);
    let error = services
        .search_occurrences_with_options(
            request.clone(),
            false,
            ServiceCallOptions::new().with_max_response_tokens(response_limit),
        )
        .await
        .expect_err("grouped coordinates must honor the serialized response bound");
    let _ = assert_response_budget_error(error, response_limit);

    let mut coordinate_request = request;
    coordinate_request.max_tokens = Some(1);
    let coordinate_only = services
        .search_occurrences(coordinate_request, true)
        .await
        .expect("coordinate-only exhaustive response");
    assert_eq!(coordinate_only.occurrences_total, 60);
    assert_eq!(coordinate_only.occurrences_returned, 60);
    assert_eq!(coordinate_only.groups_returned, 1);
    assert_eq!(coordinate_only.groups[0].occurrences.len(), 60);
    assert!(coordinate_only.groups[0].excerpt.is_none());
    assert!(coordinate_only.groups[0].content_hash.is_none());
    assert_eq!(coordinate_only.meta.source_tokens, 0);
    assert!(coordinate_only.meta.total_response_tokens < grouped.meta.total_response_tokens);
    assert_response_token_accounting!(coordinate_only, Tokenizer::default());
}

#[tokio::test]
async fn exhaustive_occurrence_search_requires_text_or_regex_mode() {
    let (_root, services) = fixture().await;
    let mut request = search_limit_request(Some(20), Some(1_000), Some(0));
    request.mode = SearchMode::Auto;
    request.all_occurrences = true;

    let error = services
        .search(request)
        .await
        .expect_err("auto mode must not claim exhaustive occurrences");

    assert!(matches!(
        error,
        Error::InvalidSearchOptions {
            field: "all_occurrences",
            ..
        }
    ));

    let mut prefer = search_limit_request(Some(20), Some(1_000), Some(0));
    prefer.mode = SearchMode::Text;
    prefer.prefer_structural = true;
    let error = services
        .search(prefer)
        .await
        .expect_err("text mode must not accept structural preference");
    assert!(matches!(
        error,
        Error::InvalidInput {
            field: "prefer structural",
            ..
        }
    ));
}

#[tokio::test]
async fn identifier_search_merges_definition_channels_and_reports_coverage() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("search.rs"),
        "fn shared_identifier() {}\nfn caller() { shared_identifier(); }\n",
    )
    .expect("source");
    std::fs::write(
        root.path().join("other.rs"),
        "fn other_caller() { shared_identifier(); }\n",
    )
    .expect("second source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let response = services
        .search(SearchRequest {
            query: "shared_identifier".into(),
            mode: SearchMode::Identifier,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: true,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("identifier search");

    assert_eq!(response.hits.len(), 1);
    let merged = &response.hits[0];
    assert_eq!(merged.match_kind, "symbol");
    assert!(merged.match_kinds.iter().any(|kind| kind == "symbol"));
    assert!(merged.match_kinds.iter().any(|kind| kind == "text"));
    assert_eq!(merged.normalized_score, 1.0);
    assert_eq!(response.coverage.definitions.total, 1);
    assert_eq!(response.coverage.definitions.returned, 1);
    assert_eq!(response.coverage.definitions.truncated, 0);
    assert!(response.coverage.references.total >= 2);
    assert_eq!(response.coverage.references.returned, 1);
    assert_eq!(
        response.coverage.references.truncated,
        response.coverage.references.total - 1
    );
    assert!(response.coverage.text_matches.total >= 1);
    assert_eq!(response.coverage.text_matches.returned, 1);
    assert_eq!(
        response.coverage.text_matches.total,
        response.coverage.text_matches.returned + response.coverage.text_matches.truncated
    );
}

#[tokio::test]
async fn exhaustive_regex_search_counts_repeated_matches_in_one_chunk() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "const values = ['item1', 'item22', 'item333'];\n";
    std::fs::write(root.path().join("regex.js"), source).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let response = services
        .search(SearchRequest {
            query: r"item\d+".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("exhaustive regex search");

    assert_eq!(response.occurrences_total, Some(3));
    assert_eq!(response.occurrences_returned, 3);
    assert_eq!(response.hits.len(), 3);
    assert!(
        response
            .hits
            .iter()
            .all(|hit| hit.occurrence.is_some() && hit.match_kind == "regex")
    );
}

#[tokio::test]
async fn regex_candidate_plans_match_full_scan_and_report_fallback_selection() {
    let root = tempfile::tempdir().expect("temporary repository");
    for (path, source) in [
        (
            "alpha.rs",
            "const needle_value: usize = 42;\nconst marker_value: usize = 7;\n",
        ),
        ("bravo.rs", "const needle_value: usize = 7;\n"),
        ("digits.rs", "const value_123: usize = 123;\n"),
        ("repeat.rs", "const repeated = \"abab\";\n"),
        ("negative.rs", "const unrelated: usize = 0;\n"),
    ] {
        std::fs::write(root.path().join(path), source).expect("write source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    for (pattern, expected_strategy, expected_source, expected_fallback) in [
        (
            r"needle_value\s*:\s*usize\s*=\s*42",
            leantoken::RegexCandidateStrategy::Trigram,
            Some(leantoken::RegexPlanSource::MandatoryLiterals),
            None,
        ),
        (
            r"(?:needle|marker)_value",
            leantoken::RegexCandidateStrategy::Trigram,
            Some(leantoken::RegexPlanSource::MandatoryLiterals),
            None,
        ),
        (
            r"(?:needle|)value",
            leantoken::RegexCandidateStrategy::Trigram,
            Some(leantoken::RegexPlanSource::MandatoryLiterals),
            None,
        ),
        (
            r"(?:needle)?\d+",
            leantoken::RegexCandidateStrategy::FullScan,
            None,
            Some(leantoken::RegexPlanFallbackReason::LiteralSequenceUnavailable),
        ),
        (
            r"needle|value_\d+",
            leantoken::RegexCandidateStrategy::Trigram,
            Some(leantoken::RegexPlanSource::MandatoryLiterals),
            None,
        ),
        (
            r"needle|\d+",
            leantoken::RegexCandidateStrategy::FullScan,
            None,
            Some(leantoken::RegexPlanFallbackReason::LiteralSequenceUnavailable),
        ),
        (
            r"(?:ab){2}",
            leantoken::RegexCandidateStrategy::Trigram,
            Some(leantoken::RegexPlanSource::PrefixLiterals),
            None,
        ),
    ] {
        let request = SearchRequest {
            query: pattern.into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(20),
            max_tokens: Some(4_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        };
        let optimized = services
            .search_evaluation(request.clone())
            .await
            .expect("optimized regex");
        let full_scan = services
            .search_full_scan_evaluation(request)
            .await
            .expect("full-scan regex");

        // Opaque IDs intentionally affect serialized accounting; this
        // assertion compares retrieval strategy output.
        let mut optimized_response = optimized.response.clone();
        optimized_response.meta.receipt_id = None;
        optimized_response.meta.path_and_metadata_tokens = 0;
        optimized_response.meta.total_response_tokens = 0;
        optimized_response.meta.total_response_tokens = 0;
        let mut full_scan_response = full_scan.response.clone();
        full_scan_response.meta.receipt_id = None;
        full_scan_response.meta.path_and_metadata_tokens = 0;
        full_scan_response.meta.total_response_tokens = 0;
        full_scan_response.meta.total_response_tokens = 0;
        assert_eq!(
            serde_json::to_value(optimized_response).expect("optimized JSON"),
            serde_json::to_value(full_scan_response).expect("full scan JSON"),
            "{pattern}"
        );
        assert_eq!(
            optimized.phases.regex_planning.strategy(),
            expected_strategy,
            "{pattern}"
        );
        assert_eq!(
            optimized.phases.regex_planning.source(),
            expected_source,
            "{pattern}"
        );
        assert_eq!(
            optimized.phases.regex_planning.fallback_reason(),
            expected_fallback,
            "{pattern}"
        );
        assert!(optimized.phases.regex_plan_nodes > 0, "{pattern}");
        assert_eq!(
            full_scan.phases.regex_planning.strategy(),
            leantoken::RegexCandidateStrategy::FullScan,
            "{pattern}"
        );
        assert_eq!(
            full_scan.phases.regex_planning.fallback_reason(),
            Some(leantoken::RegexPlanFallbackReason::PlanningDisabled),
            "{pattern}"
        );
        assert_eq!(
            optimized.phases.regex_chunks_verified,
            optimized
                .phases
                .regex_candidate_chunks
                .max(optimized.phases.regex_chunks_loaded),
            "{pattern}"
        );
        assert_eq!(
            full_scan.phases.regex_retained_chunks > 0,
            !full_scan.response.hits.is_empty(),
            "{pattern}"
        );
        assert!(
            full_scan.phases.regex_retained_chunks <= full_scan.phases.regex_chunks_loaded,
            "{pattern}"
        );
    }
}

#[tokio::test]
async fn regex_planner_reports_privacy_safe_fallback_reasons_and_budgets() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("fixture.rs"), "const needle: usize = 1;\n")
        .expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let request = |query: String, case_sensitive: bool| SearchRequest {
        query,
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(4_000),
        context_lines: Some(0),
        case_sensitive,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };

    let case_insensitive = services
        .search_evaluation(request("needle".into(), false))
        .await
        .expect("case-insensitive fallback");
    assert_eq!(
        case_insensitive.phases.regex_planning.fallback_reason(),
        Some(leantoken::RegexPlanFallbackReason::CaseInsensitiveUnicode)
    );
    assert_eq!(case_insensitive.phases.regex_plan_nodes, 0);
    assert_eq!(case_insensitive.phases.regex_plan_terms, 0);
    assert_eq!(case_insensitive.phases.regex_plan_term_bytes, 0);

    let term_limited_pattern = (0..33)
        .map(|index| format!("x{index:02}"))
        .collect::<Vec<_>>()
        .join("|");
    let term_limited = services
        .search_evaluation(request(term_limited_pattern, true))
        .await
        .expect("term-limit fallback");
    assert_eq!(
        term_limited.phases.regex_planning.fallback_reason(),
        Some(leantoken::RegexPlanFallbackReason::PlanTermLimit)
    );
    assert_eq!(term_limited.phases.regex_plan_terms, 33);
    assert!(term_limited.phases.regex_plan_term_bytes > 0);

    let bytes_limited = services
        .search_evaluation(request("z".repeat(257), true))
        .await
        .expect("term-bytes fallback");
    assert_eq!(
        bytes_limited.phases.regex_planning.fallback_reason(),
        Some(leantoken::RegexPlanFallbackReason::PlanTermBytesLimit)
    );
    assert_eq!(bytes_limited.phases.regex_plan_terms, 1);
    assert_eq!(bytes_limited.phases.regex_plan_term_bytes, 257);

    let node_limited_pattern = "[ab]".repeat(257);
    let node_limited = services
        .search_evaluation(request(node_limited_pattern, true))
        .await
        .expect("node-limit fallback");
    assert_eq!(
        node_limited.phases.regex_planning.fallback_reason(),
        Some(leantoken::RegexPlanFallbackReason::PlanNodeLimit)
    );
    assert_eq!(node_limited.phases.regex_plan_nodes, 257);
    assert_eq!(node_limited.phases.regex_plan_terms, 0);
    assert_eq!(node_limited.phases.regex_plan_term_bytes, 0);
}

#[tokio::test]
async fn regex_candidate_plan_preserves_candidate_limit_errors() {
    let root = tempfile::tempdir().expect("temporary repository");
    for index in 0..21 {
        std::fs::write(
            root.path().join(format!("match_{index:02}.rs")),
            format!("const overflow_needle_{index:02}: usize = {index};\n"),
        )
        .expect("write source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let request = SearchRequest {
        query: "overflow_needle".into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(1),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };

    let optimized = services
        .search_evaluation(request.clone())
        .await
        .expect_err("optimized candidate cap");
    let full_scan = services
        .search_full_scan_evaluation(request)
        .await
        .expect_err("full-scan candidate cap");

    for error in [optimized, full_scan] {
        assert!(matches!(
            error,
            Error::RetrievalLimitExceeded {
                kind: leantoken::RetrievalLimitKind::RegexRetainedChunks,
                observed: 21,
                limit: 20,
            }
        ));
    }
}

#[tokio::test]
async fn regex_full_scan_reports_the_per_file_chunk_bound_without_a_path() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = "let value = true;\n".repeat(257 * 80);
    std::fs::write(root.path().join("large.rs"), source).expect("write source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");
    let request = SearchRequest {
        query: r"\d+".into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(4_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };

    let error = services
        .search_evaluation(request)
        .await
        .expect_err("full scan must reject the oversized file");

    assert!(matches!(
        error,
        Error::RetrievalLimitExceeded {
            kind: leantoken::RetrievalLimitKind::RegexChunksPerFile,
            observed: 257,
            limit: 256,
        }
    ));
}

#[tokio::test]
async fn regex_candidate_plan_applies_path_scope_before_candidate_limit() {
    let root = tempfile::tempdir().expect("temporary repository");
    let included = root.path().join("included");
    std::fs::create_dir(&included).expect("create included directory");
    std::fs::write(
        included.join("match.rs"),
        "const scoped_overflow_needle: usize = 42;\n",
    )
    .expect("write source");
    let database = root.path().join("index.sqlite");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    let mut connection = rusqlite::Connection::open(database).expect("writer connection");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 40
             )
             INSERT INTO files(path, content_hash, generation)
             SELECT printf('excluded/%02d.rs', value), 'dummy', 1 FROM sequence;

             WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 250
             )
             INSERT INTO chunks(
                 file_id, content, start_line, end_line,
                 start_byte, end_byte, token_count
             )
             SELECT f.id, 'scoped_overflow_needle', sequence.value, sequence.value,
                    0, 22, 1
             FROM files f
             CROSS JOIN sequence
             WHERE f.path GLOB 'excluded/*';",
        )
        .expect("populate excluded candidates");
    transaction.commit().expect("commit candidates");

    let request = SearchRequest {
        query: "scoped_overflow_needle".into(),
        mode: SearchMode::Regex,
        include_paths: vec!["included/**".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let optimized = services
        .search_evaluation(request.clone())
        .await
        .expect("scoped candidate plan");
    let full_scan = services
        .search_full_scan_evaluation(request)
        .await
        .expect("scoped full scan");

    // Opaque IDs intentionally affect serialized accounting; this assertion
    // compares retrieval strategy output.
    let mut optimized_response = optimized.response.clone();
    optimized_response.meta.receipt_id = None;
    optimized_response.meta.path_and_metadata_tokens = 0;
    optimized_response.meta.total_response_tokens = 0;
    optimized_response.meta.total_response_tokens = 0;
    let mut full_scan_response = full_scan.response.clone();
    full_scan_response.meta.receipt_id = None;
    full_scan_response.meta.path_and_metadata_tokens = 0;
    full_scan_response.meta.total_response_tokens = 0;
    full_scan_response.meta.total_response_tokens = 0;
    assert_eq!(
        serde_json::to_value(optimized_response).expect("optimized JSON"),
        serde_json::to_value(full_scan_response).expect("full-scan JSON")
    );
    assert_eq!(optimized.phases.regex_candidate_chunks, 1);
    assert_eq!(optimized.phases.regex_chunks_verified, 1);
}

#[tokio::test]
async fn regex_candidate_plan_bypasses_only_the_full_scan_file_bound() {
    let root = tempfile::tempdir().expect("temporary repository");
    let database = root.path().join("index.sqlite");
    std::fs::write(
        root.path().join("match.rs"),
        "const openclaw_scale_needle: usize = 42;\n",
    )
    .expect("write source");
    let config = Config::discover(root.path(), Some(database.clone())).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index fixture");

    // Populate the relational file inventory without creating 10,000 physical
    // files. Only the indexed source owns a chunk, which isolates whether a
    // sound candidate query is incorrectly gated by the fallback scan bound.
    let mut connection = rusqlite::Connection::open(database).expect("writer connection");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 10000
             )
             INSERT INTO files(path, content_hash, generation)
             SELECT printf('dummy/%05d.rs', value), 'dummy', 1 FROM sequence;",
        )
        .expect("populate large file inventory");
    transaction.commit().expect("commit inventory");

    let planned_request = SearchRequest {
        query: "openclaw_scale_needle".into(),
        mode: SearchMode::Regex,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(20),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: false,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let optimized = services
        .search_evaluation(planned_request.clone())
        .await
        .expect("sound candidate plan should not scan the file inventory");
    assert_eq!(optimized.response.hits.len(), 1);
    assert_eq!(
        optimized.phases.regex_planning.strategy(),
        leantoken::RegexCandidateStrategy::Trigram
    );
    assert_eq!(optimized.phases.regex_files_considered, 10_001);
    assert_eq!(optimized.phases.regex_candidate_chunks, 1);
    assert_eq!(optimized.phases.regex_chunks_verified, 1);
    assert_eq!(optimized.phases.regex_chunks_loaded, 0);

    let full_scan = services
        .search_full_scan_evaluation(planned_request)
        .await
        .expect_err("full scan remains bounded by the file inventory");
    assert!(matches!(
        full_scan,
        Error::RetrievalLimitExceeded {
            kind: leantoken::RetrievalLimitKind::RegexFullScanFiles,
            observed: 10_001,
            limit: 10_000,
        }
    ));

    let fallback = services
        .search_evaluation(SearchRequest {
            query: "openclaw_scale_needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(20),
            max_tokens: Some(1_000),
            context_lines: Some(0),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect_err("case-insensitive fallback remains bounded");
    assert!(matches!(
        fallback,
        Error::RetrievalLimitExceeded {
            kind: leantoken::RetrievalLimitKind::RegexFullScanFiles,
            observed: 10_001,
            limit: 10_000,
        }
    ));

    let mut connection =
        rusqlite::Connection::open(root.path().join("index.sqlite")).expect("writer connection");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM sequence WHERE value < 10000
             )
             INSERT INTO chunks(
                 file_id, content, start_line, end_line, start_byte, end_byte, token_count
             )
             SELECT files.id, 'openclaw_scale_needle', value, value, 0, 22, 1
             FROM sequence
             JOIN files ON files.path = 'dummy/00001.rs';",
        )
        .expect("populate candidate overflow");
    transaction.commit().expect("commit candidate overflow");
    let candidate_overflow = services
        .search_evaluation(SearchRequest {
            query: "openclaw_scale_needle".into(),
            mode: SearchMode::Regex,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(100),
            max_tokens: Some(1_000),
            context_lines: Some(0),
            case_sensitive: true,
            all_occurrences: true,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect_err("planned candidate query remains bounded");
    assert!(matches!(
        candidate_overflow,
        Error::RetrievalLimitExceeded {
            kind: leantoken::RetrievalLimitKind::RegexCandidateChunks,
            observed: 10_001,
            limit: 10_000,
        }
    ));
}
