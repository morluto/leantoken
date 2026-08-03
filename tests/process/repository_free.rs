use super::support::Command;

pub(super) fn setup_and_remove_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");

    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .current_dir(temp.path())
        .args([
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
        .env_remove("npm_lifecycle_event")
        .current_dir(temp.path())
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

pub(super) fn repository_options_are_rejected_by_repository_free_commands() {
    for arguments in [
        vec!["--json", "--root", ".", "setup", "--all", "--dry-run"],
        vec!["--json", "--root", ".", "remove", "--all", "--dry-run"],
        vec!["--json", "cache", "list", "--max-file-bytes", "1"],
        vec![
            "--json",
            "runtime",
            "prune",
            "--yes",
            "--database",
            "ignored.sqlite",
        ],
        vec![
            "--json",
            "--root",
            ".",
            "episode",
            "audit",
            "--adapter",
            "mcp-wire-report-v2",
            "--input",
            "trace.json",
        ],
        vec!["--json", "upgrade", "--tokenizer", "cl100k_base", "--check"],
    ] {
        let output = Command::cargo_bin("leantoken")
            .expect("binary")
            .args(arguments)
            .output()
            .expect("run command");
        assert!(!output.status.success());
        let error: serde_json::Value =
            serde_json::from_slice(&output.stderr).expect("structured parse error");
        assert_eq!(error["category"], "invalid_input");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains("repository option")),
            "{error}"
        );
    }
}

pub(super) fn episode_audit_is_repo_free_deterministic_and_read_only() {
    let temp = tempfile::tempdir().expect("temporary working directory");
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks/reports/multi-agent-context-suite-v1-codex-0.144.1.json");
    let before = std::fs::read(&input).expect("read input before audit");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command.current_dir(temp.path());
        command
    };
    let arguments = [
        "--json",
        "episode",
        "audit",
        "--adapter",
        "multi-agent-suite-v1",
        "--input",
        input.to_str().expect("input UTF-8"),
    ];
    let first = command().args(arguments).output().expect("first JSON audit");
    let second = command().args(arguments).output().expect("second JSON audit");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    let report: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("normalized report");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["report_kind"], "episode_audit");
    assert_eq!(report["adapter"]["name"], "multi_agent_suite");
    assert_eq!(report["summary"]["episodes"], 60);
    assert!(
        report["findings"]
            .as_array()
            .is_some_and(|findings| findings.iter().any(|finding| {
                finding["code"] == "provider_input_regression"
                    && finding["value"]
                        .as_f64()
                        .is_some_and(|value| (value - 0.509_299_914_852_593_3).abs() < 1e-12)
            }))
    );
    assert!(!String::from_utf8_lossy(&first.stdout).contains("flask-ipv6"));
    assert_eq!(
        std::fs::read(&input).expect("read input after audit"),
        before
    );
    assert_eq!(
        std::fs::read_dir(temp.path())
            .expect("temporary directory")
            .count(),
        0
    );

    let human = command()
        .args([
            "episode",
            "audit",
            "--adapter",
            "multi-agent-suite-v1",
            "--input",
            input.to_str().expect("input UTF-8"),
        ])
        .output()
        .expect("Markdown audit");
    assert!(human.status.success());
    let markdown = String::from_utf8(human.stdout).expect("Markdown UTF-8");
    assert!(markdown.starts_with("# LeanToken episode audit\n"));
    assert!(markdown.contains("| `provider_input_regression` | 20 | 50.93% |"));
}

pub(super) fn setup_requires_yes_before_non_interactive_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex"])
        .output()
        .expect("run setup");
    assert!(!output.status.success());
    assert!(!temp.path().join(".codex/config.toml").exists());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured setup error");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("requires explicit client flags"))
    );
    assert_eq!(error["category"], "invalid_request");
}

// Windows ProjectDirs uses the Known Folder API and cannot be redirected to a
// disposable cache root through per-process environment variables. The cache
// module tests cover Windows lease and deletion semantics without user data.
#[cfg(not(windows))]
pub(super) fn cache_list_and_prune_do_not_require_a_repository() {
    let temp = tempfile::tempdir().expect("temporary home");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env_remove("npm_lifecycle_event")
            .env("XDG_CACHE_HOME", temp.path().join("xdg-cache"))
            .env("LOCALAPPDATA", temp.path().join("local-app-data"));
        command
    };
    let listed = command()
        .args(["--json", "cache", "list"])
        .output()
        .expect("list caches");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let list: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("cache list JSON");
    let cache_root = std::path::PathBuf::from(list["cache_root"].as_str().expect("cache root"));
    let cache = cache_root.join("0000000000000001");
    std::fs::create_dir_all(&cache).expect("cache directory");
    let database = cache.join("index.sqlite");
    std::fs::write(&database, b"corrupt managed cache").expect("cache fixture");
    let legacy_cache = cache_root.join("0000000000000002");
    std::fs::create_dir_all(&legacy_cache).expect("legacy cache directory");
    let legacy_database = legacy_cache.join("index.sqlite");
    rusqlite::Connection::open(&legacy_database)
        .expect("legacy database")
        .execute_batch(
            "CREATE TABLE meta (
                id INTEGER PRIMARY KEY,
                schema_version INTEGER NOT NULL,
                repository_root TEXT NOT NULL
            );
            INSERT INTO meta VALUES (1, 4, '');",
        )
        .expect("legacy metadata");

    let human_list = command()
        .args(["cache", "list"])
        .output()
        .expect("human cache list");
    assert!(human_list.status.success());
    let human_list = String::from_utf8_lossy(&human_list.stdout);
    assert!(human_list.contains("corrupt"));
    assert!(human_list.contains("last_access="));
    assert!(human_list.contains("root_available="));
    assert!(human_list.contains("scope=full"));

    let summary = command()
        .args([
            "--json",
            "cache",
            "list",
            "--summary",
            "--state",
            "corrupt",
        ])
        .output()
        .expect("cache summary");
    assert!(summary.status.success());
    let summary: serde_json::Value =
        serde_json::from_slice(&summary.stdout).expect("cache summary JSON");
    assert_eq!(summary["total_entries"], 2);
    assert_eq!(summary["matched_entries"], 1);
    assert_eq!(summary["returned_entries"], 0);
    assert_eq!(summary["state_counts"]["corrupt"], 1);
    assert_eq!(summary["entries"].as_array().map(Vec::len), Some(0));

    let dry_run = command()
        .args([
            "--json",
            "cache",
            "prune",
            "--max-total-bytes",
            "1",
            "--dry-run",
        ])
        .output()
        .expect("dry-run prune");
    assert!(dry_run.status.success());
    let dry_run: serde_json::Value =
        serde_json::from_slice(&dry_run.stdout).expect("prune JSON");
    assert_eq!(dry_run["results"][0]["action"], "kept");
    assert_eq!(dry_run["results"][1]["action"], "would_delete");
    assert!(database.exists());
    assert!(legacy_database.exists());

    let human_prune = command()
        .args([
            "cache",
            "prune",
            "--max-total-bytes",
            "1",
            "--dry-run",
        ])
        .output()
        .expect("human prune plan");
    assert!(human_prune.status.success());
    assert!(String::from_utf8_lossy(&human_prune.stdout).contains("would_delete"));

    let prune = command()
        .args([
            "--json",
            "cache",
            "prune",
            "--max-total-bytes",
            "1",
            "--yes",
        ])
        .output()
        .expect("prune cache");
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let prune: serde_json::Value =
        serde_json::from_slice(&prune.stdout).expect("prune JSON");
    assert_eq!(prune["results"][0]["action"], "kept");
    assert_eq!(prune["results"][1]["action"], "deleted");
    assert!(database.exists());
    assert!(!legacy_database.exists());
}
