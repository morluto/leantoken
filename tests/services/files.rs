use super::*;

#[tokio::test]
async fn file_operations_page_without_duplicates() {
    let root = tempfile::tempdir().expect("root");
    for name in ["alpha.rs", "bravo.rs", "charlie.rs", "delta.rs", "echo.rs"] {
        std::fs::write(
            root.path().join(name),
            format!("fn {}() {{}}\n", &name[..name.len() - 3]),
        )
        .expect("source");
    }
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    for operation in [
        FileOperation::Tree,
        FileOperation::Glob,
        FileOperation::Find,
    ] {
        let mut cursor = None;
        let mut paths = Vec::new();
        loop {
            let response = services
                .files(FilesRequest {
                    operation: operation.clone(),
                    path: None,
                    query: matches!(operation, FileOperation::Find).then(|| "rs".into()),
                    pattern: matches!(operation, FileOperation::Glob).then(|| "*.rs".into()),
                    max_results: Some(2),
                    cursor,
                    depth: Some(1),
                })
                .await
                .expect("file page");
            paths.extend(response.entries.into_iter().map(|entry| entry.path));
            cursor = response.meta.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let unique = paths.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 5, "{operation:?}");
        assert_eq!(unique.len(), paths.len(), "{operation:?}");
    }

    let tree = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: None,
            query: None,
            pattern: None,
            max_results: Some(2),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("tree page");
    let error = services
        .files(FilesRequest {
            operation: FileOperation::Glob,
            path: None,
            query: None,
            pattern: Some("*.rs".into()),
            max_results: Some(2),
            cursor: tree.meta.next_cursor,
            depth: None,
        })
        .await
        .expect_err("cursor from another operation");
    assert!(matches!(error, Error::StaleCursor));
}

#[tokio::test]
async fn fuzzy_find_ties_prioritize_production_source_and_preserve_pagination() {
    let root = tempfile::tempdir().expect("root");
    let paths = [
        "benchmarks/kotlin/src/main.rs",
        "fixtures/sample/src/go/main.go",
        "fixtures/sample/src/python/main.py",
        "fixtures/sample/src/rust/main.rs",
        "src/main.rs",
        "src/main/dispatch.rs",
        "src/main/output.rs",
        "xtask/src/main.rs",
    ];
    for path in paths {
        let source = root.path().join(path);
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("directories");
        std::fs::write(source, "fn main() {}\n").expect("source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");
    let request = |cursor, max_results| FilesRequest {
        operation: FileOperation::Find,
        path: None,
        query: Some("main".into()),
        pattern: None,
        max_results: Some(max_results),
        cursor,
        depth: None,
    };

    let full = services.files(request(None, 20)).await.expect("full find");
    assert!(
        full.entries.iter().all(|entry| entry.score == Some(109.0)),
        "fixture must exercise the equal-score tie breaker: {:?}",
        full.entries
            .iter()
            .map(|entry| (&entry.path, entry.score))
            .collect::<Vec<_>>()
    );
    let full_paths = full
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(full_paths.first().map(String::as_str), Some("src/main.rs"));
    let first_support = full_paths
        .iter()
        .position(|path| path.starts_with("benchmarks/") || path.starts_with("fixtures/"))
        .expect("support result");
    assert!(
        full_paths[..first_support]
            .iter()
            .all(|path| path.starts_with("src/") || path.starts_with("xtask/src/"))
    );
    let paths_projection = services
        .files_paths(request(None, 20))
        .await
        .expect("paths projection");
    assert_eq!(paths_projection.paths, full_paths);

    let mut cursor = None;
    let mut paged = Vec::new();
    loop {
        let page = services
            .files(request(cursor, 2))
            .await
            .expect("paged find");
        paged.extend(page.entries.into_iter().map(|entry| entry.path));
        cursor = page.meta.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(paged, full_paths);
}

#[tokio::test]
async fn files_glob_selective_pattern_returns_only_matching_paths() {
    let root = tempfile::tempdir().expect("root");
    for (name, body) in [
        ("alpha.rs", "fn alpha() {}\n"),
        ("bravo.rs", "fn bravo() {}\n"),
        ("target_one.rs", "fn target_one() {}\n"),
        ("target_two.rs", "fn target_two() {}\n"),
        ("other.txt", "plain text\n"),
    ] {
        std::fs::write(root.path().join(name), body).expect("source");
    }
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let response = services
        .files(FilesRequest {
            operation: FileOperation::Glob,
            path: None,
            query: None,
            pattern: Some("target_*.rs".into()),
            max_results: Some(10),
            cursor: None,
            depth: None,
        })
        .await
        .expect("selective glob");
    let paths = response
        .entries
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["target_one.rs", "target_two.rs"]);
}

#[tokio::test]
async fn file_tree_projection_respects_root_depth_and_removes_empty_directories() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src/deep")).expect("directories");
    std::fs::write(root.path().join("src/top.rs"), "fn top() {}\n").expect("top source");
    std::fs::write(root.path().join("src/deep/lib.rs"), "fn deep() {}\n").expect("deep source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    let tree = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("src".into()),
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(1),
        })
        .await
        .expect("tree");
    assert_eq!(
        tree.entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src", "src/deep", "src/top.rs"]
    );

    std::fs::remove_file(root.path().join("src/deep/lib.rs")).expect("delete deep source");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("refresh deletion");
    let after = services
        .files(FilesRequest {
            operation: FileOperation::Tree,
            path: Some("src".into()),
            query: None,
            pattern: None,
            max_results: Some(20),
            cursor: None,
            depth: Some(2),
        })
        .await
        .expect("tree after deletion");
    assert!(after.entries.iter().all(|entry| entry.path != "src/deep"));
}

#[tokio::test]
async fn file_tree_normalizes_equivalent_roots_before_query_and_pagination() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src/rust")).expect("directories");
    std::fs::write(root.path().join("README.md"), "fixture\n").expect("readme");
    std::fs::write(root.path().join("src/lib.rs"), "fn lib() {}\n").expect("lib source");
    std::fs::write(root.path().join("src/rust/a.rs"), "fn a() {}\n").expect("a source");
    std::fs::write(root.path().join("src/rust/b.rs"), "fn b() {}\n").expect("b source");
    let services = Services::open(
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config"),
    )
    .expect("services");
    services
        .refresh(leantoken::IndexingMode::Reconcile)
        .await
        .expect("index");

    for aliases in [
        vec![None, Some(""), Some("."), Some("./")],
        vec![Some("src"), Some("./src"), Some("src/")],
        vec![Some("src/rust"), Some("./src//rust"), Some("src/rust/")],
    ] {
        let expected = tree_pages(&services, aliases[0]).await;
        assert!(expected.len() > 1, "fixture must exercise pagination");
        for alias in aliases.into_iter().skip(1) {
            assert_eq!(
                tree_pages(&services, alias).await,
                expected,
                "alias {alias:?}"
            );
        }
    }
}

#[tokio::test]
async fn invalid_focus_glob_is_a_typed_error() {
    let (_root, services) = fixture().await;
    let error = services
        .search(SearchRequest {
            query: "greet".into(),
            mode: SearchMode::Auto,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: vec!["[".into()],
            max_results: None,
            max_tokens: None,
            context_lines: None,
            case_sensitive: false,
            all_occurrences: false,
            prefer_structural: false,
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect_err("invalid glob must fail");
    assert!(error.to_string().contains("glob"));
}

#[tokio::test]
async fn file_tree_rejects_unsafe_roots() {
    let (_root, services) = fixture().await;
    for path in ["/src", "../src", "src/../rust", "src\0rust"] {
        services
            .files(FilesRequest {
                operation: FileOperation::Tree,
                path: Some(path.into()),
                query: None,
                pattern: None,
                max_results: None,
                cursor: None,
                depth: None,
            })
            .await
            .expect_err("unsafe tree root must fail");
    }
}
