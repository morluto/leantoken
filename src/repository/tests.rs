use std::fs;

use super::*;

#[test]
fn discovery_progress_is_aggregate_monotonic_and_reports_completion() {
    let directory = tempfile::tempdir().expect("directory");
    for index in 0..(DISCOVERY_PROGRESS_INTERVAL_ENTRIES + 10) {
        fs::write(
            directory.path().join(format!("file-{index:03}.rs")),
            "fn fixture() {}\n",
        )
        .expect("fixture");
    }
    let mut snapshots = Vec::new();

    let result = discover_files_with_limits_policy_filter_and_progress(
        directory.path(),
        DiscoveryLimits::default(),
        DiscoveryPolicy::default(),
        &CancellationToken::new(),
        |_| true,
        |stats| snapshots.push(stats),
    )
    .expect("discovery");

    assert!(
        snapshots
            .windows(2)
            .all(|pair| pair[0].walk_entries <= pair[1].walk_entries)
    );
    assert_eq!(snapshots.last(), Some(&result.stats));
    assert!(
        snapshots
            .iter()
            .any(|stats| stats.walk_entries == DISCOVERY_PROGRESS_INTERVAL_ENTRIES)
    );
}

#[test]
fn discovery_reports_walker_errors_instead_of_returning_partial_results() {
    let directory = tempfile::tempdir().expect("directory");
    let missing = directory.path().join("missing");

    let error = discover_files(&missing, 1024).expect_err("missing root must fail");

    assert!(matches!(error, Error::RepositoryTraversal(_)));
}

#[test]
fn discovery_reports_metadata_errors_instead_of_skipping_entries() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("vanishing.rs");
    fs::write(&path, "fn vanishing() {}").expect("fixture");
    let entry = WalkBuilder::new(directory.path())
        .build()
        .filter_map(std::result::Result::ok)
        .find(|entry| entry.path() == path)
        .expect("file entry");
    fs::remove_file(&path).expect("remove fixture");

    let error = entry_metadata(&entry).expect_err("missing metadata must fail");

    assert!(matches!(error, Error::RepositoryTraversal(_)));
}

#[test]
fn checked_slash_path_rejects_non_utf8_paths_without_lossy_aliases() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    for name in [b"\x80.rs".to_vec(), b"\x81.rs".to_vec()] {
        let path = PathBuf::from(OsString::from_vec(name));
        let error = checked_slash_path(&path).expect_err("non-UTF-8 path must be rejected");

        match error {
            Error::UnsupportedPathEncoding(rejected) => assert_eq!(rejected, path),
            other => panic!("unexpected error: {other}"),
        }
    }
}
