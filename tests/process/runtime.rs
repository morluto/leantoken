use super::support::Command;

pub(super) fn setup_dry_run_reports_exact_plan_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex", "--dry-run"])
        .output()
        .expect("run setup dry-run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("dry-run JSON output");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["plan"][0]["client"], "codex");
    assert_eq!(report["plan"][0]["action"], "create");
    assert_eq!(report["launcher"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert!(!temp.path().join(".codex/config.toml").exists());
}

pub(super) fn malformed_selected_config_blocks_all_setup_writes() {
    let temp = tempfile::tempdir().expect("temporary home");
    std::fs::write(temp.path().join(".claude.json"), "{ broken").expect("write malformed config");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--claude", "--cursor", "--yes"])
        .output()
        .expect("run setup");
    assert!(!output.status.success());
    assert!(!temp.path().join(".cursor/mcp.json").exists());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured setup error");
    assert_eq!(error["category"], "setup_failure");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("refusing to overwrite malformed config"))
    );
    assert_eq!(error.as_object().map(serde_json::Map::len), Some(2));
}

pub(super) fn npx_setup_registers_exact_release_instead_of_its_cache_path() {
    let temp = tempfile::tempdir().expect("temporary home");
    let runtime = temp.path().join("node runtime");
    let node = runtime.join(if cfg!(windows) { "node.exe" } else { "node" });
    let npx = runtime.join("npx cli.js");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", &node)
        .env("npm_execpath", &npx)
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run npx setup");
    assert!(
        setup.status.success(),
        "ambient lifecycle metadata must not break a persistent executable"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(report["verification"]["status"], "passed");
    let executable = assert_cmd::cargo::cargo_bin!("leantoken");
    assert_eq!(report["launcher"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert_eq!(report["launcher"]["command"], executable.to_str().unwrap());
    assert_eq!(
        report["launcher"]["args"],
        serde_json::json!(["--managed-by-setup", "mcp"])
    );

    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(temp.path().join(".claude.json")).expect("Claude configuration"),
    )
    .expect("Claude JSON");
    assert_eq!(
        config["mcpServers"]["leantoken"]["command"],
        executable.to_str().unwrap()
    );
    assert_eq!(
        config["mcpServers"]["leantoken"]["args"],
        serde_json::json!(["--managed-by-setup", "mcp"])
    );
}

pub(super) fn setup_refresh_targets_only_existing_mcp_entries() {
    let temp = tempfile::tempdir().expect("temporary home");
    let node = temp.path().join("node");
    let npm = temp.path().join("npm-cli.js");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("npm_lifecycle_event", "npx")
            .env("npm_node_execpath", &node)
            .env("npm_execpath", &npm);
        command
    };
    let setup = command()
        .args(["--json", "setup", "--claude", "--yes"])
        .output()
        .expect("run initial setup");
    assert!(
        setup.status.success(),
        "ambient lifecycle metadata must not break a persistent executable"
    );
    let setup_report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    assert_eq!(setup_report["verification"]["status"], "passed");
    std::fs::create_dir_all(temp.path().join(".cursor")).expect("Cursor directory");
    std::fs::write(
        temp.path().join(".cursor/mcp.json"),
        "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n",
    )
    .expect("Cursor config");

    let refresh = command()
        .args(["--json", "setup", "--refresh", "--yes"])
        .output()
        .expect("run setup refresh");
    assert!(refresh.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&refresh.stdout).expect("refresh JSON output");
    assert_eq!(report["verification"]["status"], "passed");
    assert_eq!(report["plan"].as_array().unwrap().len(), 1);
    assert_eq!(report["plan"][0]["client"], "claude");
    assert_eq!(report["plan"][0]["action"], "already_current");
    let cursor = std::fs::read_to_string(temp.path().join(".cursor/mcp.json"))
        .expect("Cursor config after refresh");
    assert!(!cursor.contains("\"leantoken\""));
}

pub(super) fn private_runtime_setup_installs_and_registers_the_verified_native_binary() {
    let temp = tempfile::tempdir().expect("temporary home");
    let data_home = temp.path().join("data");
    let runtime = temp.path().join("node runtime");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("XDG_DATA_HOME", &data_home)
        .env("LOCALAPPDATA", &data_home)
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", runtime.join("node"))
        .env("npm_execpath", runtime.join("npm-cli.js"))
        .args(["--json", "setup", "--codex", "--private-runtime", "--yes"])
        .output()
        .expect("run private runtime setup");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&setup.stdout).expect("setup JSON output");
    let runtime_path = std::path::PathBuf::from(
        report["launcher"]["runtime_path"]
            .as_str()
            .expect("runtime path"),
    );
    assert!(runtime_path.exists());
    assert_eq!(report["launcher"]["package"], serde_json::Value::Null);
    assert_eq!(report["launcher"]["may_contact_network"], false);
    assert_eq!(report["discovery_plan"].as_array().map(Vec::len), Some(1));

    let version = Command::new(&runtime_path)
        .arg("--version")
        .output()
        .expect("run installed native executable");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    let repository = temp.path().join("workspace");
    std::fs::create_dir(&repository).expect("workspace");
    std::fs::write(
        repository.join("lib.rs"),
        "pub fn private_runtime_retrieval() -> bool { true }\n",
    )
    .expect("workspace source");
    let doctor = Command::new(&runtime_path)
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("XDG_DATA_HOME", &data_home)
        .env("LOCALAPPDATA", &data_home)
        .args([
            "--root",
            repository.to_str().expect("repository UTF-8"),
            "--database",
            temp.path()
                .join("private-runtime.sqlite")
                .to_str()
                .expect("database UTF-8"),
            "--json",
            "doctor",
        ])
        .output()
        .expect("run installed runtime doctor");
    assert!(
        doctor.status.success(),
        "private runtime doctor stderr: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor report");
    assert_eq!(
        doctor_report["tools"],
        serde_json::json!([
            "context",
            "files",
            "history",
            "json",
            "outline",
            "read",
            "receipt_rebase",
            "savings",
            "search"
        ])
    );
    assert_eq!(doctor_report["first_call"]["status"], "ready");
    let codex = std::fs::read_to_string(temp.path().join(".codex/config.toml"))
        .expect("Codex configuration");
    assert!(codex.contains(runtime_path.to_str().expect("UTF-8 runtime path")));
    assert!(!codex.contains("npm-cli"));
}

pub(super) fn npx_setup_explains_that_it_does_not_install_a_global_cli() {
    let temp = tempfile::tempdir().expect("temporary home");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("npm_lifecycle_event", "npx")
        .env("npm_node_execpath", temp.path().join("node"))
        .env("npm_execpath", temp.path().join("npm-cli.js"))
        .args(["setup", "--codex", "--yes"])
        .output()
        .expect("run npx setup");

    assert!(
        output.status.success(),
        "ambient lifecycle metadata must not break a persistent executable"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LeanToken // Context Distillery"));
    assert!(stdout.contains("LeanToken is configured for 1 client."));
    assert!(stdout.contains("✓ Exact launcher verified: initialize, 9-tool catalog"));
    assert!(stdout.contains(
        "Verify the stored Codex launcher from a repository: leantoken doctor --client codex"
    ));
    assert!(stdout.contains("Update later with: leantoken upgrade"));
    assert!(!stdout.contains("Some selected clients failed"));
    assert!(!stdout.contains("Launcher verification failed"));
}
