use assert_cmd::Command;

#[test]
fn cli_indexes_statuses_and_searches_as_json() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let index = run(root.path(), &database, &["index"]);
    assert!(
        index["files_indexed"]
            .as_u64()
            .is_some_and(|value| value >= 1)
    );

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["file_count"], 1);

    let search = run(
        root.path(),
        &database,
        &[
            "search",
            "answer",
            "--mode",
            "identifier",
            "--max-tokens",
            "100",
        ],
    );
    assert_eq!(search["hits"][0]["path"], "lib.rs");
    assert!(
        search["meta"]["emitted_tokens"]
            .as_u64()
            .is_some_and(|value| value <= 100)
    );
}

#[test]
fn mcp_exits_cleanly_on_stdio_eof() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");

    Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "mcp",
        ])
        .write_stdin("")
        // The deadline covers cold indexing and watcher startup as well as
        // transport shutdown, which is materially slower on Windows runners.
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success();
}

#[test]
fn setup_and_remove_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");
    let missing_root = temp.path().join("not-a-repository");

    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args([
            "--root",
            missing_root.to_str().expect("root UTF-8"),
            "--json",
            "setup",
            "--claude",
            "--yes",
        ])
        .output()
        .expect("run setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(report["results"][0]["status"], "configured");
    let config = std::fs::read_to_string(temp.path().join(".claude.json"))
        .expect("Claude configuration");
    assert!(config.contains("\"leantoken\""));
    assert!(config.contains("\"mcp\""));

    let remove = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .args(["--json", "remove", "--claude", "--yes"])
        .output()
        .expect("run remove");
    assert!(
        remove.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    let config = std::fs::read_to_string(temp.path().join(".claude.json"))
        .expect("Claude configuration after removal");
    assert!(!config.contains("\"leantoken\""));
}

fn run(
    root: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&str],
) -> serde_json::Value {
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
        ])
        .args(arguments)
        .output()
        .expect("run leantoken");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}
