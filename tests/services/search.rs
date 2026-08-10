use super::*;

#[tokio::test]
async fn search_range_covers_the_returned_context_lines() {
    let (_root, services) = fixture().await;
    let response = services
        .search(SearchRequest {
            query: "agent".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(100),
            context_lines: Some(1),
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("search");

    let hit = response.hits.first().expect("text hit");
    assert_eq!((hit.start_line, hit.end_line), (5, 7));
    assert_eq!(hit.excerpt.lines().count(), 3);
    assert_eq!(hit.enclosing_symbol.as_deref(), Some("caller"));
}

#[tokio::test]
async fn token_limited_search_cursor_defers_a_hit_without_skipping_it() {
    let (_root, services) = indexed_source("paged.txt", b"needle\nneedle\n").await;
    let request = SearchRequest {
        query: "needle".into(),
        mode: SearchMode::Text,
        include_paths: vec!["paged.txt".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(1),
        max_tokens: Some(100),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor: None,
    };
    let one_hit = services.search(request.clone()).await.expect("one hit");
    let mut paged = request;
    paged.max_results = Some(2);
    paged.max_tokens = Some(one_hit.meta.source_tokens);

    let first = services.search(paged.clone()).await.expect("first page");
    assert_eq!(first.hits.len(), 1);
    assert_eq!(first.occurrences_total, Some(2));
    let first_line = first.hits[0].start_line;
    paged.cursor = first.meta.next_cursor;

    let second = services.search(paged).await.expect("second page");
    assert_eq!(second.hits.len(), 1);
    assert_ne!(second.hits[0].start_line, first_line);
    assert_eq!(second.meta.next_cursor, None);
}

#[tokio::test]
async fn token_limited_search_skips_unfit_hits_while_advancing_the_cursor() {
    let source = format!(
        "needle {}\nneedle {}\n",
        "context ".repeat(40),
        "context ".repeat(40)
    );
    let (_root, services) = indexed_source("oversized.txt", source.as_bytes()).await;

    let request = SearchRequest {
        query: "needle".into(),
        mode: SearchMode::Text,
        include_paths: vec!["oversized.txt".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(1),
        max_tokens: Some(1),
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
        .expect("an unfit result is a valid empty page");
    assert!(first.hits.is_empty());
    let cursor = first
        .meta
        .next_cursor
        .expect("the next candidate remains reachable");

    let final_page = services
        .search(SearchRequest {
            cursor: Some(cursor),
            ..request
        })
        .await
        .expect("the final unfit result remains a valid page");
    assert!(final_page.hits.is_empty());
    assert_eq!(final_page.meta.next_cursor, None);
}

#[tokio::test]
async fn text_search_windows_keep_case_insensitive_matches_across_a_chunk() {
    let mut lines = (1..=60)
        .map(|line| format!("ordinary line {line}"))
        .collect::<Vec<_>>();
    let cases = [
        (30usize, "MiddleNeedle"),
        (59usize, "LateNeedle"),
        (2usize, "EarlyNeedle"),
    ];
    for (line, needle) in cases {
        lines[line - 1] = format!("{needle} is anchored here");
    }
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("positions.txt", source.as_bytes()).await;

    for (match_line, needle) in cases {
        let response = services
            .search(SearchRequest {
                query: needle.to_ascii_lowercase(),
                mode: SearchMode::Text,
                include_paths: vec!["positions.txt".into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(1),
                max_tokens: Some(1_000),
                context_lines: Some(20),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            })
            .await
            .expect("case-insensitive text search");

        let hit = response.hits.first().expect("text hit");
        assert!(
            hit.excerpt.contains(needle),
            "excerpt for line {match_line} omitted {needle}: {:?}",
            hit.excerpt
        );
        assert_eq!(hit.match_kind, "text");
        assert!(hit.start_line <= match_line && hit.end_line >= match_line);
        assert_eq!(
            hit.end_line - hit.start_line + 1,
            hit.excerpt.lines().count()
        );
        assert_eq!(hit.excerpt.lines().count(), 20);
    }
}

#[tokio::test]
async fn maximum_text_context_keeps_the_original_read_bounded_range_match() {
    let mut lines = (1..=50)
        .map(|line| format!("// legacy source line {line}"))
        .collect::<Vec<_>>();
    lines[29] = "fn read_bounded_range() {}".into();
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("legacy.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "read_bounded_range".into(),
            mode: SearchMode::Text,
            include_paths: vec!["legacy.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("legacy reproduction search");

    let hit = response.hits.first().expect("legacy text hit");
    assert!(hit.excerpt.contains("read_bounded_range"));
    assert!(hit.start_line <= 30 && hit.end_line >= 30);
}

#[tokio::test]
async fn short_text_queries_match_inside_longer_tokens() {
    let source = b"alpha prefixfnordsuffix omega\n";
    let (_root, services) = indexed_source("short.txt", source).await;

    for mode in [SearchMode::Text, SearchMode::Auto] {
        for query in ["f", "fn"] {
            let response = services
                .search(SearchRequest {
                    query: query.into(),
                    mode,
                    include_paths: vec!["short.txt".into()],
                    exclude_paths: Vec::new(),
                    focus_paths: Vec::new(),
                    max_results: Some(1),
                    max_tokens: Some(1_000),
                    context_lines: Some(1),
                    case_sensitive: true,
                    all_occurrences: false,
                    prefer_structural: false,
                    receipt_id: None,
                    query_receipt: None,
                    cursor: None,
                })
                .await
                .expect("short text search");

            let hit = response.hits.first().expect("embedded substring hit");
            assert_eq!(hit.path, "short.txt");
            assert_eq!(hit.match_kind, "text");
            assert!(hit.excerpt.contains("prefixfnordsuffix"));
        }
    }
}

#[tokio::test]
async fn short_identifier_queries_match_inside_longer_tokens() {
    let source = b"alpha prefixfnordsuffix omega\n";
    let (_root, services) = indexed_source("short.txt", source).await;

    let response = services
        .search(SearchRequest {
            query: "fn".into(),
            mode: SearchMode::Identifier,
            include_paths: vec!["short.txt".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("short identifier search");

    let hit = response.hits.first().expect("embedded substring hit");
    assert_eq!(hit.path, "short.txt");
    assert_eq!(hit.match_kind, "text");
    assert!(hit.excerpt.contains("prefixfnordsuffix"));
}

#[tokio::test]
async fn regex_search_keeps_a_multiline_match_that_exceeds_the_line_cap() {
    let mut lines = (1..=5)
        .map(|line| format!("prefix {line}"))
        .collect::<Vec<_>>();
    lines.push("MATCH_BEGIN".into());
    lines.extend((1..=24).map(|line| format!("matched body {line}")));
    lines.push("MATCH_END".into());
    lines.extend((1..=5).map(|line| format!("suffix {line}")));
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("multiline.txt", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "(?s)MATCH_BEGIN.*?MATCH_END".into(),
            mode: SearchMode::Regex,
            include_paths: vec!["multiline.txt".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(5_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("multiline regex search");

    let hit = response.hits.first().expect("regex hit");
    assert!(hit.excerpt.contains("MATCH_BEGIN"));
    assert!(hit.excerpt.contains("MATCH_END"));
    assert_eq!((hit.start_line, hit.end_line), (6, 31));
    assert_eq!(
        hit.end_line - hit.start_line + 1,
        hit.excerpt.lines().count()
    );
    assert_eq!(hit.excerpt.lines().count(), 26);
}

#[tokio::test]
async fn symbol_search_caps_a_long_definition_without_losing_its_declaration() {
    let mut lines = (1..=20)
        .map(|line| format!("const PREFIX_{line}: usize = {line};"))
        .collect::<Vec<_>>();
    let declaration_line = lines.len() + 1;
    lines.push("fn long_target() -> usize {".into());
    lines.extend((1..=40).map(|line| format!("    let value_{line} = {line};")));
    lines.push("    40".into());
    lines.push("}".into());
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("long_symbol.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "long_target".into(),
            mode: SearchMode::Symbol,
            include_paths: vec!["long_symbol.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(2_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("long symbol search");

    let hit = response.hits.first().expect("symbol hit");
    assert!(hit.excerpt.contains("fn long_target()"));
    assert!(hit.start_line <= declaration_line && hit.end_line >= declaration_line);
    assert_eq!(hit.excerpt.lines().count(), 30);
    assert_eq!(hit.end_line - hit.start_line + 1, 30);
}

#[tokio::test]
async fn reference_search_window_keeps_the_required_reference_span() {
    let mut lines = vec![
        "fn target() {}".to_string(),
        String::new(),
        "fn caller() {".into(),
    ];
    lines.extend((1..=25).map(|line| format!("    let value_{line} = {line};")));
    let reference_line = lines.len() + 1;
    lines.push("    target();".into());
    lines.push("}".into());
    let source = format!("{}\n", lines.join("\n"));
    let (_root, services) = indexed_source("reference.rs", source.as_bytes()).await;

    let response = services
        .search(SearchRequest {
            query: "target".into(),
            mode: SearchMode::Reference,
            include_paths: vec!["reference.rs".into()],
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(1),
            max_tokens: Some(1_000),
            context_lines: Some(20),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("reference search");

    let hit = response.hits.first().expect("reference hit");
    assert!(hit.excerpt.contains("target();"));
    assert!(hit.start_line <= reference_line && hit.end_line >= reference_line);
    assert_eq!(
        hit.end_line - hit.start_line + 1,
        hit.excerpt.lines().count()
    );
    assert_eq!(hit.excerpt.lines().count(), 12);
}

#[tokio::test]
async fn text_search_reports_enclosing_symbols_across_languages() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "fn rust_owner() {\n    let known_hashes: Vec<String> = Vec::new();\n}\n",
    )
    .expect("Rust source");
    std::fs::write(
        root.path().join("owner.py"),
        "def python_owner():\n    known_hashes = []\n    return known_hashes\n",
    )
    .expect("Python source");
    std::fs::write(
        root.path().join("owner.js"),
        "function javascriptOwner() {\n  const known_hashes = [];\n  return known_hashes;\n}\n",
    )
    .expect("JavaScript source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let response = services
        .search(SearchRequest {
            query: "known_hashes".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("search");
    let owners = response
        .hits
        .into_iter()
        .map(|hit| (hit.path, hit.enclosing_symbol))
        .collect::<std::collections::HashMap<_, _>>();

    assert_eq!(
        owners.get("owner.rs").and_then(Option::as_deref),
        Some("rust_owner")
    );
    assert_eq!(
        owners.get("owner.py").and_then(Option::as_deref),
        Some("python_owner")
    );
    assert_eq!(
        owners.get("owner.js").and_then(Option::as_deref),
        Some("javascriptOwner")
    );
}

#[tokio::test]
async fn text_search_preserves_multiline_matches_without_a_single_matching_line() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("owner.rs"),
        "fn multiline_owner() {\n    first_line();\n    second_line();\n}\n",
    )
    .expect("Rust source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let response = services
        .search(SearchRequest {
            query: "first_line();\n    second_line();".into(),
            mode: SearchMode::Text,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(1_000),
            context_lines: Some(1),
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("search");

    let hit = response.hits.first().expect("multiline text hit");
    assert_eq!(hit.path, "owner.rs");
    assert!(hit.excerpt.contains("first_line();\n    second_line();"));
    assert_eq!(hit.enclosing_symbol.as_deref(), Some("multiline_owner"));
}

#[tokio::test]
async fn case_insensitive_search_uses_the_verifier_unicode_case_folding() {
    let source = "const LABEL: &str = \"Აbc\";\nfn Აbc() {}\nfn caller() { Აbc(); }\n";
    let (root, services) = indexed_source("unicode.rs", source.as_bytes()).await;

    for (mode, expected_kind) in [
        (SearchMode::Text, "text"),
        (SearchMode::Identifier, "symbol"),
        (SearchMode::Symbol, "symbol"),
        (SearchMode::Reference, "reference"),
    ] {
        let response = services
            .search(SearchRequest {
                query: "აbc".into(),
                mode,
                include_paths: vec!["unicode.rs".into()],
                exclude_paths: Vec::new(),
                focus_paths: Vec::new(),
                max_results: Some(10),
                max_tokens: Some(1_000),
                context_lines: Some(1),
                case_sensitive: false,
                all_occurrences: false,
                prefer_structural: false,
                receipt_id: None,
                query_receipt: None,
                cursor: None,
            })
            .await
            .expect("Unicode case-insensitive search");

        let hit = response
            .hits
            .iter()
            .find(|hit| hit.match_kind == expected_kind)
            .unwrap_or_else(|| panic!("missing {expected_kind} hit in {mode:?}"));
        assert_eq!(hit.path, "unicode.rs");
        assert!(
            hit.excerpt.contains("Აbc"),
            "{mode:?} returned unrelated content: {:?}",
            hit.excerpt
        );
        if matches!(mode, SearchMode::Symbol | SearchMode::Reference) {
            assert!(
                hit.score_reasons
                    .iter()
                    .any(|reason| reason.starts_with("exact ")),
                "Unicode-equivalent structural identity was not scored as exact: {:?}",
                hit.score_reasons
            );
        }
        if mode == SearchMode::Identifier {
            assert!(
                response
                    .hits
                    .iter()
                    .any(|hit| hit.match_kinds.iter().any(|kind| kind == "text")),
                "identifier word-FTS omitted the Unicode-equivalent lexical hit: {:?}",
                response.hits
            );
        }
    }

    std::fs::write(
        root.path().join("unicode.txt"),
        "plain text contains Აbc without structural records\n",
    )
    .expect("write plain-text Unicode fixture");
    services
        .index(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index plain-text Unicode fixture");
    let context = services
        .context(ContextRequest {
            task: "აbc".into(),
            token_budget: 400,
            include_paths: vec!["unicode.txt".into()],
            must_include_paths: Vec::new(),
            must_include_symbols: Vec::new(),
            required_evidence: Vec::new(),
            max_fragments: Some(4),
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
            explain_diagnostics: false,
        })
        .await
        .expect("Unicode case-insensitive context");
    assert!(
        context
            .fragments
            .iter()
            .any(|fragment| fragment.path == "unicode.txt" && fragment.content.contains("Აbc")),
        "context omitted Unicode-equivalent source: {:?}",
        context.fragments
    );
}
