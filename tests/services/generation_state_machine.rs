use super::*;

fn read_request() -> ReadRequest {
    ReadRequest {
        path: "state.rs".into(),
        start_line: Some(1),
        end_line: Some(3),
        symbol: None,
        heading: None,
        heading_occurrence: None,
        continuation_cursor: None,
        max_tokens: Some(1_000),
        expected_hash: None,
        delta: false,
        receipt_id: None,
        policy: leantoken::ReadPolicy::Full,
    }
}

fn search_request(cursor: Option<String>) -> SearchRequest {
    SearchRequest {
        query: "needle".into(),
        mode: SearchMode::Text,
        include_paths: vec!["state.rs".into()],
        exclude_paths: Vec::new(),
        focus_paths: Vec::new(),
        max_results: Some(1),
        max_tokens: Some(1_000),
        context_lines: Some(0),
        case_sensitive: true,
        all_occurrences: true,
        prefer_structural: false,
        receipt_id: None,
        query_receipt: None,
        cursor,
    }
}

#[tokio::test]
async fn refresh_query_cancel_page_restart_state_machine() {
    let root = tempfile::tempdir().expect("repository root");
    let database = root.path().join("index.sqlite");
    let source = root.path().join("state.rs");
    std::fs::write(
        &source,
        "fn old() {\n    needle();\n    needle();\n    needle();\n}\n",
    )
    .expect("source");
    let config = Config::discover(root.path(), Some(database)).expect("configuration");
    let services = Services::open(config.clone()).expect("services");

    assert!(matches!(
        services.read(read_request()).await,
        Err(Error::IndexNotReady)
    ));
    let first_publish = services.refresh().await.expect("first publish");
    let first = services.read(read_request()).await.expect("first read");
    assert_eq!(
        first.meta.repository_generation,
        first_publish.repository_generation
    );
    assert!(
        first
            .content
            .as_deref()
            .is_some_and(|text| text.contains("old"))
    );

    let first_page = services
        .search(search_request(None))
        .await
        .expect("first page");
    let old_cursor = first_page.meta.next_cursor.expect("continuation");

    std::fs::write(&source, "fn dirty() {\n    needle();\n}\n").expect("dirty edit");
    let stable = services.read(read_request()).await.expect("stable read");
    assert_eq!(stable.content_hash, first.content_hash);
    assert_eq!(
        stable.meta.repository_generation,
        first.meta.repository_generation
    );

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        services.refresh_cancellable(cancelled).await,
        Err(Error::Cancelled)
    ));
    let after_cancel = services
        .read(read_request())
        .await
        .expect("read after cancel");
    assert_eq!(after_cancel.content_hash, first.content_hash);

    let second_publish = services.refresh().await.expect("second publish");
    assert!(second_publish.repository_generation > first.meta.repository_generation);
    assert!(matches!(
        services.search(search_request(Some(old_cursor))).await,
        Err(Error::StaleCursor)
    ));
    let second = services.read(read_request()).await.expect("second read");
    assert!(
        second
            .content
            .as_deref()
            .is_some_and(|text| text.contains("dirty"))
    );

    let generation = second.meta.repository_generation;
    let hash = second.content_hash.clone();
    drop(services);
    let restarted = Services::open(config).expect("restart services");
    let after_restart = restarted.read(read_request()).await.expect("restart read");
    assert_eq!(after_restart.meta.repository_generation, generation);
    assert_eq!(after_restart.content_hash, hash);
}

#[tokio::test]
async fn every_dirty_query_prefix_observes_only_the_last_published_model_state() {
    for queries_before_refresh in 0..=4 {
        let root = tempfile::tempdir().expect("repository root");
        let source = root.path().join("state.rs");
        std::fs::write(&source, "fn generation_zero() {}\n").expect("source");
        let config = Config::discover(root.path(), Some(root.path().join("index.sqlite")))
            .expect("configuration");
        let services = Services::open(config).expect("services");
        services.refresh().await.expect("initial publish");
        let published = services.read(read_request()).await.expect("published read");

        std::fs::write(&source, "fn generation_one() {}\n").expect("edit");
        for _ in 0..queries_before_refresh {
            let observed = services.read(read_request()).await.expect("model query");
            assert_eq!(observed.content_hash, published.content_hash);
            assert_eq!(
                observed.meta.repository_generation,
                published.meta.repository_generation
            );
        }

        services.refresh().await.expect("publish edit");
        let observed = services
            .read(read_request())
            .await
            .expect("new model state");
        assert_ne!(observed.content_hash, published.content_hash);
    }
}
