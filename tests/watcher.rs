use std::time::Duration;

use tokio_util::sync::CancellationToken;

use leantoken::watcher::RepositoryWatcher;

#[tokio::test]
async fn watcher_reports_file_change() {
    let root = tempfile::tempdir().expect("root");
    let token = CancellationToken::new();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(50),
        token.child_token(),
    )
    .await
    .expect("start watcher");

    // Write a file and wait for the watcher to debounce and emit a message.
    std::fs::write(root.path().join("changed.rs"), "fn changed() {}\n").expect("write");

    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("watcher should emit within 5s")
        .expect("channel open");

    match msg {
        leantoken::watcher::WatcherMessage::Changed { paths } => {
            assert!(
                paths.iter().any(|p| p.contains("changed.rs")),
                "expected changed.rs in {:?}",
                paths
            );
        }
        leantoken::watcher::WatcherMessage::ReconcileRequired => {}
    }

    watcher.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn watcher_shutdown_cancels_task() {
    let root = tempfile::tempdir().expect("root");
    let token = CancellationToken::new();
    let (watcher, mut rx) = RepositoryWatcher::start(
        root.path(),
        64,
        Duration::from_millis(50),
        token.child_token(),
    )
    .await
    .expect("start watcher");

    watcher.shutdown().await.expect("shutdown");

    // Channel should close after shutdown.
    let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}
