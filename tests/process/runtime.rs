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
    let executable = assert_cmd::cargo::cargo_bin!("leantoken")
        .canonicalize()
        .expect("canonical executable");
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

pub(super) fn empty_setup_refresh_reports_unrecognized_clients_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary home");
    let config = temp.path().join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).expect("Codex directory");
    let original_config =
        "[mcp_servers.leantoken]\ncommand = \"/opt/manual-leantoken\"\nargs = [\"mcp\"]\n";
    std::fs::write(&config, original_config).expect("Codex configuration");
    let discovery = temp.path().join(".agents/skills/leantoken/SKILL.md");
    std::fs::create_dir_all(discovery.parent().unwrap()).expect("discovery directory");
    let original_discovery = "<!-- managed by leantoken setup -->\nlegacy discovery\n";
    std::fs::write(&discovery, original_discovery).expect("discovery skill");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env_remove("npm_lifecycle_event")
        .args(["setup", "--refresh", "--yes"])
        .output()
        .expect("run setup refresh");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "No existing LeanToken client registrations were recognized; no changes were made."
    ));
    assert_eq!(std::fs::read_to_string(config).unwrap(), original_config);
    assert_eq!(
        std::fs::read_to_string(discovery).unwrap(),
        original_discovery
    );
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

fn assert_human_runtime_list(output: &[u8]) {
    let output = String::from_utf8_lossy(output);
    assert!(output.contains("Private runtime root:"));
    assert!(output.contains("4 runtime(s)"));
    assert!(output.contains("referenced_by=Claude Code"));
    assert!(output.contains("inactive,unrecognized"));
}

#[cfg(not(windows))]
fn assert_human_runtime_prune(output: &[u8]) {
    let output = String::from_utf8_lossy(output);
    assert!(output.contains("Private runtime prune dry-run:"));
    assert!(output.contains("would_remove  1.0.0  3 bytes  outside_retention"));
    assert!(output.contains("retained  0.9.0  6 bytes  unrecognized_directory_contents"));
}

#[cfg(not(windows))]
fn assert_runtime_inventory(listed: &serde_json::Value) {
    assert_eq!(listed["total_entries"], 4);
    let entries = listed["entries"].as_array().expect("runtime entries");
    let referenced_entry = entries
        .iter()
        .find(|entry| entry["version"] == "1.1.0")
        .expect("referenced runtime");
    assert_eq!(
        referenced_entry["referenced_by"],
        serde_json::json!(["claude"])
    );
    let unsafe_entry = entries
        .iter()
        .find(|entry| entry["version"] == "0.9.0")
        .expect("unsafe runtime");
    assert_eq!(unsafe_entry["safely_prunable"], false);
}

#[cfg(not(windows))]
fn assert_runtime_prune_decisions(planned: &serde_json::Value) {
    let results = planned["results"].as_array().expect("prune decisions");
    assert!(results.iter().any(|result| {
        result["version"] == "1.1.0"
            && result["action"] == "retained"
            && result["reason"] == "referenced_by_client"
    }));
    assert!(results.iter().any(|result| {
        result["version"] == "0.9.0"
            && result["action"] == "retained"
            && result["reason"] == "unrecognized_directory_contents"
    }));
}

#[cfg(unix)]
pub(super) fn runtime_commands_refuse_a_symlinked_runtime_root_without_mutation() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary home");
    let data_home = temp.path().join("data");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("npm_lifecycle_event");
        command
    };
    let initial = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("initial runtime list");
    assert!(initial.status.success());
    let initial: serde_json::Value =
        serde_json::from_slice(&initial.stdout).expect("initial runtime JSON");
    let runtime_root =
        std::path::PathBuf::from(initial["runtime_root"].as_str().expect("runtime root"));
    let external = temp.path().join("external-runtimes");
    let version = external.join("1.0.0");
    std::fs::create_dir_all(&version).expect("external runtime directory");
    let executable = version.join("leantoken");
    std::fs::write(&executable, "external").expect("external runtime");
    std::fs::create_dir_all(runtime_root.parent().expect("runtime root parent"))
        .expect("runtime parent");
    symlink(&external, &runtime_root).expect("symlink runtime root");

    let list = command()
        .args(["runtime", "list"])
        .output()
        .expect("list symlinked runtime root");
    assert!(!list.status.success());
    assert!(String::from_utf8_lossy(&list.stderr).contains("non-symlink directory"));
    let prune = command()
        .args(["runtime", "prune", "--keep-latest", "0", "--yes"])
        .output()
        .expect("prune symlinked runtime root");
    assert!(!prune.status.success());
    assert!(String::from_utf8_lossy(&prune.stderr).contains("non-symlink directory"));
    let setup = command()
        .args(["setup", "--codex", "--private-runtime", "--yes"])
        .output()
        .expect("setup through symlinked runtime root");
    assert!(!setup.status.success());
    assert!(String::from_utf8_lossy(&setup.stderr).contains("non-symlink directory"));
    assert!(executable.exists());
    assert!(!external.join("setup.lock").exists());
    assert!(!temp.path().join(".codex/config.toml").exists());
}

#[cfg(not(windows))]
pub(super) fn runtime_list_and_prune_are_bounded_reference_safe_and_dry_run_by_default() {
    let temp = tempfile::tempdir().expect("temporary home");
    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
            .env_remove("npm_lifecycle_event");
        command
    };
    let initial = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("initial runtime list");
    assert!(initial.status.success());
    let initial: serde_json::Value =
        serde_json::from_slice(&initial.stdout).expect("initial runtime JSON");
    let runtime_root =
        std::path::PathBuf::from(initial["runtime_root"].as_str().expect("runtime root"));
    let executable_name = if cfg!(windows) {
        "leantoken.exe"
    } else {
        "leantoken"
    };
    let runtime = |version: &str, bytes: &[u8]| {
        let directory = runtime_root.join(version);
        std::fs::create_dir_all(&directory).expect("runtime directory");
        let executable = directory.join(executable_name);
        std::fs::write(&executable, bytes).expect("runtime executable");
        executable
    };
    let oldest = runtime("1.0.0", b"old");
    let referenced = runtime("1.1.0", b"referenced");
    let newest = runtime("1.2.0", b"newest");
    let unsafe_runtime = runtime("0.9.0", b"unsafe");
    std::fs::write(unsafe_runtime.parent().unwrap().join("notes.txt"), b"keep")
        .expect("unrecognized sibling");
    let referenced_alias = referenced
        .parent()
        .unwrap()
        .join("..")
        .join("1.1.0")
        .join(executable_name);
    assert_eq!(
        referenced_alias.canonicalize().unwrap(),
        referenced.canonicalize().unwrap()
    );
    let claude = serde_json::json!({"mcpServers": {"leantoken": {
        "command": referenced_alias, "args": ["--managed-by-setup", "mcp"]
    }}});
    std::fs::write(
        temp.path().join(".claude.json"),
        serde_json::to_vec(&claude).unwrap(),
    )
    .expect("Claude config");

    let listed = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("runtime list");
    assert!(listed.status.success());
    let listed: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("runtime list JSON");
    assert_runtime_inventory(&listed);
    let human_list = command()
        .args(["runtime", "list"])
        .output()
        .expect("human runtime list");
    assert!(human_list.status.success());
    assert_human_runtime_list(&human_list.stdout);

    let planned = command()
        .args(["--json", "runtime", "prune", "--keep-latest", "0"])
        .output()
        .expect("runtime prune plan");
    assert!(planned.status.success());
    let planned: serde_json::Value =
        serde_json::from_slice(&planned.stdout).expect("runtime prune JSON");
    assert_eq!(planned["dry_run"], true);
    assert!(oldest.exists() && newest.exists() && referenced.exists() && unsafe_runtime.exists());
    assert_runtime_prune_decisions(&planned);
    let human_plan = command()
        .args(["runtime", "prune", "--keep-latest", "0"])
        .output()
        .expect("human runtime prune plan");
    assert!(human_plan.status.success());
    assert_human_runtime_prune(&human_plan.stdout);

    let applied = command()
        .args(["--json", "runtime", "prune", "--keep-latest", "0", "--yes"])
        .output()
        .expect("apply runtime prune");
    assert!(
        applied.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(!oldest.exists());
    assert!(!newest.exists());
    assert!(referenced.exists());
    assert!(unsafe_runtime.exists());
    let final_list = command()
        .args(["--json", "runtime", "list"])
        .output()
        .expect("final runtime list");
    assert!(final_list.status.success());
    let final_list: serde_json::Value =
        serde_json::from_slice(&final_list.stdout).expect("final runtime JSON");
    assert_eq!(final_list["ignored_entries"], 0);

    std::fs::create_dir_all(temp.path().join(".cursor")).expect("Cursor config directory");
    let oversized = std::fs::File::create(temp.path().join(".cursor/mcp.json"))
        .expect("oversized Cursor config");
    oversized
        .set_len(8 * 1024 * 1024 + 1)
        .expect("extend Cursor config");
    let bounded = command()
        .args(["runtime", "list"])
        .output()
        .expect("bounded runtime list");
    assert!(!bounded.status.success());
    assert!(String::from_utf8_lossy(&bounded.stderr).contains("byte limit"));
}

pub(super) fn ambient_npx_metadata_does_not_replace_the_persistent_setup_launcher() {
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
    let executable = assert_cmd::cargo::cargo_bin!("leantoken")
        .canonicalize()
        .expect("canonical executable");
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

pub(super) fn ambient_npx_metadata_keeps_the_persistent_setup_handoff() {
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
