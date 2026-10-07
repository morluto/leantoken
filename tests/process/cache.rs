use super::support::Command;
use std::fs;

pub(super) fn cli_cache_compact_previews_applies_and_reports_reader_failures() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    fs::write(
        repository.join("main.rs"),
        "pub fn compact_probe() -> i32 { 42 }\n",
    )
    .unwrap();
    let command = || {
        let mut command = Command::cargo_bin("leantoken").unwrap();
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("LOCALAPPDATA", temp.path().join("local-app-data"))
            .env("SQLITE_TMPDIR", temp.path())
            .env("TMPDIR", temp.path())
            .env("TMP", temp.path())
            .env("TEMP", temp.path())
            .env_remove("npm_lifecycle_event")
            .current_dir(temp.path())
            .timeout(std::time::Duration::from_secs(30));
        command
    };
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
    let list = command()
        .args(["--json", "cache", "list"])
        .output()
        .unwrap();
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
