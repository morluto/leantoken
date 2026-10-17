use super::support::Command;
use std::fs;

fn cache_fixture_command(
    executable: impl AsRef<std::ffi::OsStr>,
    home: &std::path::Path,
) -> Command {
    let mut command = Command::new(executable);
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("LOCALAPPDATA", home.join("local-app-data"))
        .env("SQLITE_TMPDIR", home)
        .env("TMPDIR", home)
        .env("TMP", home)
        .env("TEMP", home)
        .env_remove("npm_lifecycle_event")
        .current_dir(home)
        .timeout(std::time::Duration::from_secs(30));
    command
}

#[test]
fn public_cache_library_maintenance_preserves_consent_and_cancellation() {
    const CHILD_HOME: &str = "LEANTOKEN_TEST_PUBLIC_CACHE_API_CHILD_HOME";
    const MANIFEST: &str = "public-cache-api-fixture.json";
    const COMPLETED: &str = "public cache library scenario completed";

    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = std::path::PathBuf::from(home);
        assert_eq!(
            std::env::var_os("HOME"),
            Some(home.clone().into_os_string())
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(home.join(MANIFEST)).unwrap()).unwrap();
        run_public_cache_library_scenario(&home, &manifest);
        println!("{COMPLETED}");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    fs::write(
        repository.join("main.rs"),
        "fn public_cache_api_probe() {}\n",
    )
    .unwrap();
    let indexed = cache_fixture_command(assert_cmd::cargo::cargo_bin!("leantoken"), temp.path())
        .args([
            "--root",
            repository.to_str().unwrap(),
            "--tokenizer",
            "estimate",
            "index",
        ])
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let inventory = cache_fixture_command(assert_cmd::cargo::cargo_bin!("leantoken"), temp.path())
        .args(["--json", "cache", "list"])
        .output()
        .unwrap();
    assert!(
        inventory.status.success(),
        "{}",
        String::from_utf8_lossy(&inventory.stderr)
    );
    fs::write(temp.path().join(MANIFEST), &inventory.stdout).unwrap();

    // Use the harness's exact name, which omits the integration crate prefix.
    // The completion marker also prevents a zero-test child from passing.
    let test_name = std::thread::current()
        .name()
        .expect("test harness name")
        .to_owned();
    let output = cache_fixture_command(std::env::current_exe().unwrap(), temp.path())
        .env(CHILD_HOME, temp.path())
        .args(["--exact", &test_name, "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(COMPLETED),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

struct PublicCacheFixture {
    id: String,
    database: std::path::PathBuf,
    before: Vec<u8>,
    total_bytes: u64,
}

fn run_public_cache_library_scenario(home: &std::path::Path, manifest: &serde_json::Value) {
    let fixture = assert_public_cache_list_contract(home, manifest);
    let (confirmed, preview) = assert_public_cache_consent_and_preview(&fixture);
    assert_public_cache_cancellation(&fixture, &confirmed);
    assert_public_cache_deletion(&fixture, &confirmed, &preview);
}

fn assert_public_cache_list_contract(
    home: &std::path::Path,
    manifest: &serde_json::Value,
) -> PublicCacheFixture {
    use leantoken::cache::{self, CacheCompatibility, CacheListRequest, CacheState};

    let listed = cache::list_with(&CacheListRequest::default()).unwrap();
    assert!(
        listed
            .cache_root
            .canonicalize()
            .unwrap()
            .starts_with(home.canonicalize().unwrap())
    );
    assert_eq!(
        listed.cache_root.to_str().unwrap(),
        manifest["cache_root"].as_str().unwrap()
    );
    assert_eq!(listed.total_entries, 1);
    assert_eq!(listed.matched_entries, 1);
    let entry = &listed.entries()[0].entry;
    assert_eq!(entry.id, manifest["entries"][0]["id"].as_str().unwrap());
    assert_eq!(entry.state, CacheState::Current);
    assert!(!entry.active);
    let database = entry.path.join("index.sqlite");
    let before = fs::read(&database).unwrap();
    let filtered_request = CacheListRequest {
        states: vec![CacheState::Current],
        repository_root: entry.repository_root.clone(),
        compatibilities: vec![CacheCompatibility::CompatibleCurrent],
        index_content_versions: vec![entry.index_content_version.unwrap()],
        limit: 1,
        ..Default::default()
    };
    let filtered = cache::list_with(&filtered_request).unwrap();
    assert_eq!(filtered.returned_entries(), 1);
    assert_eq!(filtered.entries()[0].entry.id, entry.id);
    assert_eq!(filtered.total_bytes, listed.total_bytes);
    assert_eq!(filtered.matched_bytes, listed.total_bytes);
    assert_eq!(filtered.state_counts["current"], 1);
    assert_eq!(
        filtered.compatibility_counts["compatible_current"].entries,
        1
    );
    assert_eq!(
        filtered.compatibility_counts["compatible_current"].bytes,
        listed.total_bytes
    );
    let summary = cache::list_with(&CacheListRequest {
        summary: true,
        ..filtered_request.clone()
    })
    .unwrap();
    assert!(summary.summary_only());
    assert_eq!(summary.returned_entries(), 0);
    assert_eq!(summary.matched_entries, 1);
    assert_eq!(summary.matched_bytes, filtered.matched_bytes);
    assert_eq!(summary.compatibility_counts, filtered.compatibility_counts);
    let unmatched = cache::list_with(&CacheListRequest {
        repository_root: Some(home.join("unindexed-repository")),
        ..filtered_request
    })
    .unwrap();
    assert_eq!(unmatched.total_entries, 1);
    assert_eq!(unmatched.matched_entries, 0);
    assert_eq!(unmatched.returned_entries(), 0);
    assert_eq!(unmatched.matched_bytes, 0);

    PublicCacheFixture {
        id: entry.id.clone(),
        database,
        before,
        total_bytes: listed.total_bytes,
    }
}

fn assert_public_cache_consent_and_preview(
    fixture: &PublicCacheFixture,
) -> (
    leantoken::cache::CachePruneRequest,
    leantoken::cache::CachePruneReport,
) {
    use leantoken::cache::{self, CachePruneOutcome, CachePruneRequest};

    let request = CachePruneRequest {
        older_than_days: None,
        max_total_bytes: Some(0),
        remove_missing_roots: false,
        dry_run: false,
        yes: false,
        incompatible_with_current: false,
    };
    assert!(matches!(
        cache::prune(&request),
        Err(leantoken::Error::InvalidRequest(_))
    ));
    assert_eq!(fs::read(&fixture.database).unwrap(), fixture.before);
    let preview = cache::prune(&CachePruneRequest {
        dry_run: true,
        ..request.clone()
    })
    .unwrap();
    assert!(preview.dry_run);
    assert_eq!(preview.results.len(), 1);
    assert!(matches!(
        preview.results[0].outcome,
        CachePruneOutcome::WouldDelete
    ));
    assert_eq!(preview.total_bytes_before, fixture.total_bytes);
    assert_eq!(preview.reclaimed_bytes, preview.total_bytes_before);
    assert_eq!(preview.total_bytes_after, 0);
    assert_eq!(fs::read(&fixture.database).unwrap(), fixture.before);

    let confirmed = CachePruneRequest {
        yes: true,
        ..request
    };
    (confirmed, preview)
}

fn assert_public_cache_cancellation(
    fixture: &PublicCacheFixture,
    confirmed: &leantoken::cache::CachePruneRequest,
) {
    use leantoken::cache::{self, CacheListRequest};

    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        cache::list_with_cancellation(&CacheListRequest::default(), &cancellation),
        Err(leantoken::Error::Cancelled)
    ));
    assert!(matches!(
        cache::prune_with_cancellation(confirmed, &cancellation),
        Err(leantoken::Error::Cancelled)
    ));
    assert_eq!(fs::read(&fixture.database).unwrap(), fixture.before);
}

fn assert_public_cache_deletion(
    fixture: &PublicCacheFixture,
    confirmed: &leantoken::cache::CachePruneRequest,
    preview: &leantoken::cache::CachePruneReport,
) {
    use leantoken::cache::{self, CacheListRequest, CachePruneOutcome};

    let applied = cache::prune(confirmed).unwrap();
    assert!(!applied.dry_run && !applied.has_failures());
    assert_eq!(applied.results.len(), 1);
    assert!(matches!(
        applied.results[0].outcome,
        CachePruneOutcome::Deleted
    ));
    assert_eq!(applied.results[0].id, fixture.id);
    assert_eq!(applied.total_bytes_before, preview.total_bytes_before);
    assert_eq!(applied.reclaimed_bytes, applied.total_bytes_before);
    assert_eq!(applied.total_bytes_after, 0);
    assert!(!fixture.database.exists());
    let empty = cache::list_with(&CacheListRequest::default()).unwrap();
    assert_eq!(empty.total_entries, 0);
    assert_eq!(empty.total_bytes, 0);
}

#[test]
fn cli_cache_inventory_limit_prevents_partial_list_and_destructive_prune() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    fs::write(repository.join("main.rs"), "fn inventory_probe() {}\n").unwrap();
    let command = || cache_fixture_command(assert_cmd::cargo::cargo_bin!("leantoken"), temp.path());
    command()
        .args([
            "--root",
            repository.to_str().unwrap(),
            "--tokenizer",
            "estimate",
            "index",
        ])
        .assert()
        .success();
    let listed = command()
        .args(["--json", "cache", "list"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let root = std::path::Path::new(listed["cache_root"].as_str().unwrap());
    assert!(
        root.canonicalize()
            .unwrap()
            .starts_with(temp.path().canonicalize().unwrap())
    );
    let database =
        std::path::Path::new(listed["entries"][0]["path"].as_str().unwrap()).join("index.sqlite");
    let before = fs::read(&database).unwrap();
    for index in 0..4096 {
        fs::write(root.join(format!("unmanaged-{index}")), []).unwrap();
    }
    for arguments in [
        vec!["--json", "cache", "list", "--limit", "1"],
        vec!["--json", "cache", "list", "--summary"],
        vec![
            "--json",
            "cache",
            "prune",
            "--max-total-bytes",
            "0",
            "--yes",
        ],
    ] {
        let output = command().args(arguments).output().unwrap();
        assert!(
            !output.status.success(),
            "inventory must refuse an incomplete population"
        );
        assert!(
            output.stdout.is_empty(),
            "an incomplete inventory must not look complete"
        );
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["category"], "incomplete_inventory");
        assert_eq!(error["complete"], false);
        assert_eq!(error["field"], "root_entries");
        assert_eq!(error["limit"], 4096);
        assert_eq!(
            fs::read(&database).unwrap(),
            before,
            "prune must fail before deleting artifacts"
        );
    }
}

#[test]
fn cli_cache_compact_previews_applies_and_reports_reader_failures() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    fs::write(
        repository.join("main.rs"),
        "pub fn compact_probe() -> i32 { 42 }\n",
    )
    .unwrap();
    let command = || cache_fixture_command(assert_cmd::cargo::cargo_bin!("leantoken"), temp.path());
    let indexed = command()
        .args([
            "--json",
            "--root",
            repository.to_str().unwrap(),
            "--tokenizer",
            "estimate",
            "index",
        ])
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let unix_seconds = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    };
    let list_started = unix_seconds();
    let list = command()
        .args(["--json", "cache", "list"])
        .output()
        .unwrap();
    let list_finished = unix_seconds();
    assert!(list.status.success());
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let cache_root = std::path::Path::new(list["cache_root"].as_str().unwrap());
    assert!(
        cache_root
            .canonicalize()
            .unwrap()
            .starts_with(temp.path().canonicalize().unwrap()),
        "fixture cache must stay in its disposable home"
    );
    let entries = list["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let id = entries[0]["id"].as_str().unwrap();
    let index_content_version = entries[0]["index_content_version"]
        .as_u64()
        .unwrap()
        .to_string();
    let repository_root = repository.to_str().unwrap();
    let filtered_started = unix_seconds();
    let filtered = command()
        .args([
            "--json",
            "cache",
            "list",
            "--state",
            "current",
            "--compatibility",
            "compatible-current",
            "--index-content-version",
            &index_content_version,
            "--repository-root",
            repository_root,
        ])
        .output()
        .unwrap();
    let filtered_finished = unix_seconds();
    assert!(filtered.status.success());
    let filtered: serde_json::Value = serde_json::from_slice(&filtered.stdout).unwrap();
    let stable_entries = |report: &serde_json::Value, started: u64, finished: u64| {
        let mut entries = report["entries"].as_array().unwrap().clone();
        for entry in &mut entries {
            let accessed = entry["last_access_unix_seconds"].as_u64().unwrap();
            let age = entry
                .as_object_mut()
                .unwrap()
                .remove("age_seconds")
                .unwrap()
                .as_u64()
                .unwrap();
            // Each inspection recomputes age. Bound it independently, including
            // a clock rollback, before comparing the stable fields and order.
            let earliest_age = started.min(finished).saturating_sub(accessed);
            let latest_age = started.max(finished).saturating_sub(accessed);
            assert!(
                (earliest_age..=latest_age).contains(&age),
                "age {age} is outside {earliest_age}..={latest_age} for access time {accessed}"
            );
        }
        entries
    };
    assert_eq!(
        stable_entries(&filtered, filtered_started, filtered_finished),
        stable_entries(&list, list_started, list_finished)
    );

    let database = cache_root.join(id).join("index.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE compact_churn(id INTEGER PRIMARY KEY,payload BLOB); INSERT INTO compact_churn VALUES(1,zeroblob(1048576)); DELETE FROM compact_churn;").unwrap();
    let generation: i64 = connection
        .query_row(
            "SELECT repository_generation FROM meta WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    let arguments = [
        "cache",
        "compact",
        "--id",
        id,
        "--min-reclaim-bytes",
        "1",
        "--min-reclaim-percent",
        "1",
    ];
    let human = command().args(arguments).output().unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("would_compact"));
    let preview = command().arg("--json").args(arguments).output().unwrap();
    assert!(preview.status.success());
    let preview: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["results"][0]["action"], "would_compact");
    assert_eq!(preview["reclaimed_bytes"], 0);
    assert_eq!(fs::read(&database).unwrap(), before);
    #[cfg(windows)]
    {
        let outside_preview = command().arg("--json").args(arguments).output().unwrap();
        assert!(outside_preview.status.success());
        let outside_preview: serde_json::Value =
            serde_json::from_slice(&outside_preview.stdout).unwrap();
        assert_eq!(outside_preview["results"][0]["action"], "would_compact");
        assert_eq!(outside_preview["results"][0]["vacuum_committed"], false);
        let failed = command()
            .env("TMP", temp.path().join("missing-tmp"))
            .env("TEMP", temp.path())
            .arg("--json")
            .args(arguments)
            .arg("--yes")
            .output()
            .unwrap();
        assert!(!failed.status.success());
        let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
        assert_eq!(report["results"][0]["action"], "failed");
        assert_eq!(report["results"][0]["vacuum_committed"], false);
        assert_eq!(fs::read(&database).unwrap(), before);
    }
    let reader = rusqlite::Connection::open(&database).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row(
                "SELECT repository_generation FROM meta WHERE id=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        generation
    );
    let failed = command()
        .arg("--json")
        .args(arguments)
        .arg("--yes")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let error: serde_json::Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(error["category"], "cache_compact_failure");
    let failed: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["results"][0]["action"], "failed");
    assert_eq!(failed["results"][0]["vacuum_committed"], false);
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    let applied = command()
        .arg("--json")
        .args(arguments)
        .arg("--yes")
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(applied["results"][0]["action"], "compacted");
    assert!(applied["reclaimed_bytes"].as_u64().unwrap() >= 1048576);
    let connection = rusqlite::Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT repository_generation FROM meta WHERE id=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        generation
    );
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
