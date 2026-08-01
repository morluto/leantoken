use super::*;

#[cfg(unix)]
#[tokio::test]
async fn live_read_cannot_escape_through_replaced_path_components() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/module.rs"),
        "pub fn contained_source() {}\n",
    )
    .expect("contained source");
    std::fs::write(
        outside.path().join("module.rs"),
        "pub fn external_marker_needle() {}\n",
    )
    .expect("external source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    std::fs::rename(root.path().join("src"), root.path().join("src.original"))
        .expect("move indexed directory");
    symlink(outside.path(), root.path().join("src")).expect("replace directory with symlink");

    assert!(
        services
            .read(ReadRequest {
                path: "src/module.rs".into(),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                start_line: None,
                end_line: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .is_err()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn live_read_cannot_escape_through_replaced_path_components() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("src/module.rs"),
        "pub fn contained_source() {}\n",
    )
    .expect("contained source");
    std::fs::write(
        outside.path().join("module.rs"),
        "pub fn external_marker_needle() {}\n",
    )
    .expect("external source");
    let config =
        Config::discover(root.path(), Some(root.path().join("index.sqlite"))).expect("config");
    let services = Services::open(config).expect("services");
    services.index(false).await.expect("index");

    std::fs::rename(root.path().join("src"), root.path().join("src.original"))
        .expect("move indexed directory");
    let junction = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.path().join("src"))
        .arg(outside.path())
        .output()
        .expect("create junction");
    assert!(
        junction.status.success(),
        "junction creation failed: {}",
        String::from_utf8_lossy(&junction.stderr)
    );

    assert!(
        services
            .read(ReadRequest {
                path: "src/module.rs".into(),
                symbol: None,
                heading: None,
                heading_occurrence: None,
                start_line: None,
                end_line: None,
                continuation_cursor: None,
                max_tokens: Some(100),
                expected_hash: None,
                delta: false,
                receipt_id: None,
                policy: leantoken::ReadPolicy::default(),
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn repository_identity_distinguishes_linked_worktrees_before_empty_search_is_evidence() {
    require_git();

    let parent = tempfile::tempdir().expect("parent");
    let base = parent.path().join("base");
    let linked = parent.path().join("linked");
    std::fs::create_dir(&base).expect("base");
    std::fs::write(base.join("base.rs"), "pub fn base_only() {}\n").expect("base source");
    init_git_repo(&base);
    let worktree = std::process::Command::new("git")
        .args(["worktree", "add", "-b", "holdout-worktree"])
        .arg(&linked)
        .current_dir(&base)
        .output()
        .expect("git worktree add");
    assert!(
        worktree.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&worktree.stderr)
    );
    std::fs::write(
        linked.join("holdout.rs"),
        "pub fn linked_worktree_holdout_symbol() {}\n",
    )
    .expect("holdout source");

    let base_services = Services::open(
        Config::discover(&base, Some(parent.path().join("base.sqlite"))).expect("base config"),
    )
    .expect("base services");
    let linked_services = Services::open(
        Config::discover(&linked, Some(parent.path().join("linked.sqlite"))).expect("linked config"),
    )
    .expect("linked services");
    base_services.index(false).await.expect("index base");
    linked_services.index(false).await.expect("index linked");

    let base_id = base_services.repository_id();
    let linked_id = linked_services.repository_id();
    assert_ne!(base_id, linked_id);
    assert!(matches!(
        base_services.validate_repository_id(Some(&linked_id)),
        Err(Error::RepositoryIdentityMismatch { expected, actual })
            if expected == linked_id && actual == base_id
    ));
    let response = linked_services
        .search(SearchRequest {
            query: "linked_worktree_holdout_symbol".into(),
            mode: SearchMode::Symbol,
            case_sensitive: true,
            all_occurrences: false,
            prefer_structural: false,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            focus_paths: Vec::new(),
            max_results: Some(10),
            max_tokens: Some(200),
            context_lines: Some(0),
            receipt_id: None,
            query_receipt: None,
            cursor: None,
        })
        .await
        .expect("linked search");
    assert_eq!(response.meta.repository_id, linked_id);
    assert_eq!(response.hits.len(), 1);
}
