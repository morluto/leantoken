use super::support::{Command, EXPECTED_INDEX_CONTENT_VERSION, assert_runtime_version, run};

pub(super) fn doctor_verifies_identity_catalog_and_first_retrieval() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn context_distillery_ready() -> bool { true }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");

    let report = run(root.path(), &database, &["doctor"]);
    assert_eq!(report["status"], "ready");
    assert_eq!(report["server_name"], "leantoken");
    assert_runtime_version(&report["server_version"]);
    assert_eq!(
        report["index_content_version"],
        EXPECTED_INDEX_CONTENT_VERSION
    );
    assert_eq!(report["instructions_loaded"], true);
    assert_eq!(report["result_mode"], "structured");
    assert!(
        matches!(
            report["integration"]["registration_status"].as_str(),
            Some("registered" | "not_registered" | "unknown")
        ),
        "structured registration status: {report}"
    );
    assert!(
        matches!(
            report["integration"]["discovery_status"].as_str(),
            Some("installed" | "partial" | "missing" | "unknown")
        ),
        "structured discovery status: {report}"
    );
    assert_eq!(report["integration"]["launcher_status"], "healthy");
    assert_eq!(report["integration"]["handshake_status"], "healthy");
    assert_eq!(report["integration"]["catalog_status"], "healthy");
    assert!(
        report["integration"]["repair_command"]
            .as_str()
            .is_some_and(|command| command.contains("leantoken"))
    );
    assert_eq!(report["first_call"]["status"], "ready");
    assert!(
        report["first_call"]["attempts"]
            .as_u64()
            .is_some_and(|attempts| attempts >= 1)
    );
}

pub(super) fn doctor_surfaces_bounded_redacted_child_diagnostics() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let blocked_parent = root.path().join("blocked");
    std::fs::write(&blocked_parent, "not a directory").expect("blocked parent");
    let database = blocked_parent.join("index.sqlite");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "doctor",
        ])
        .output()
        .expect("run doctor");

    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured error");
    let message = error["error"].as_str().expect("error message");
    assert_eq!(error["category"], "doctor_failure");
    assert!(message.contains("child diagnostics:"), "{message}");
    assert!(message.contains("MCP indexing runtime failed"), "{message}");
    assert!(!message.contains(database.to_str().unwrap()), "{message}");
    assert!(message.len() < 5_000, "diagnostic must remain bounded");
}

pub(super) fn doctor_human_output_uses_context_distillery_handoff() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "doctor",
        ])
        .output()
        .expect("run doctor");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Context Distillery is checking"));
    assert!(stdout.contains("LeanToken // Context Distillery"));
    assert!(stdout.contains("MCP identity: leantoken"));
    assert!(stdout.contains(&format!(
        "Index compatibility: v{EXPECTED_INDEX_CONTENT_VERSION}"
    )));
    assert!(stdout.contains("Tool catalog:"));
    assert!(stdout.contains("leantoken.context first"));
}

pub(super) fn doctor_can_exercise_the_exact_codex_registration() {
    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "fn configured_doctor_ready() {}\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex", "--yes"])
        .output()
        .expect("configure Codex");
    assert!(
        setup.status.success(),
        "setup stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("npm_lifecycle_event")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run configured-client doctor");
    assert!(
        output.status.success(),
        "doctor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("doctor report");
    assert_eq!(report["integration"]["verified_client"], "codex");
    assert!(report.get("index_content_version").is_none());
    let codex = report["integration"]["registrations"]
        .as_array()
        .and_then(|registrations| {
            registrations
                .iter()
                .find(|registration| registration["client"] == "codex")
        })
        .expect("Codex registration");
    assert_eq!(codex["managed"], true);
    assert_eq!(report["first_call"]["status"], "ready");
}

pub(super) fn configured_doctor_launches_workspace_relative_commands_from_the_workspace() {
    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "fn configured_doctor_ready() {}\n",
    )
    .expect("write fixture");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("create workspace bin");
    let executable_name = if cfg!(windows) {
        "leantoken.exe"
    } else {
        "leantoken"
    };
    std::fs::copy(
        assert_cmd::cargo::cargo_bin!("leantoken"),
        bin.join(executable_name),
    )
    .expect("copy workspace launcher");
    let config = home.path().join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("create config parent");
    std::fs::write(
        &config,
        format!(
            "[mcp_servers.leantoken]\ncommand = \"./bin/{executable_name}\"\nargs = [\"--managed-by-setup\", \"mcp\"]\n"
        ),
    )
    .expect("write relative Codex registration");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            root.path()
                .join("index.sqlite")
                .to_str()
                .expect("database UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run configured doctor");

    assert!(
        output.status.success(),
        "doctor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("doctor report");
    assert_eq!(report["integration"]["verified_client"], "codex");
    assert_eq!(report["first_call"]["status"], "ready");
}

pub(super) fn configured_doctor_isolates_selected_client_from_unrelated_config_errors() {
    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "fn configured_doctor_ready() {}\n",
    )
    .expect("write fixture");
    let setup = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("npm_lifecycle_event")
        .args(["--json", "setup", "--codex", "--yes"])
        .output()
        .expect("configure Codex");
    assert!(setup.status.success());
    std::fs::write(home.path().join(".claude.json"), "{ broken")
        .expect("write unrelated malformed config");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("npm_lifecycle_event")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run configured-client doctor");

    assert!(
        output.status.success(),
        "doctor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("doctor report");
    assert_eq!(report["integration"]["verified_client"], "codex");
    assert_eq!(report["integration"]["registration_status"], "unknown");
    assert!(
        report["integration"]["registrations"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
}

pub(super) fn configured_doctor_maps_malformed_client_config_to_registration_stage() {
    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let config = home.path().join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("create config parent");
    std::fs::write(config, "[broken").expect("write malformed config");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run configured doctor");

    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured doctor error");
    assert_eq!(error["category"], "doctor_failure");
    assert_eq!(error["stage"], "registration");
}

pub(super) fn configured_doctor_rejects_a_disabled_opencode_registration() {
    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let config = home.path().join(".config/opencode/opencode.json");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("create config parent");
    let executable = assert_cmd::cargo::cargo_bin!("leantoken")
        .canonicalize()
        .expect("canonical executable");
    std::fs::write(
        config,
        serde_json::to_vec(&serde_json::json!({
            "mcp": { "leantoken": {
                "type": "local", "command": [executable, "--managed-by-setup", "mcp"],
                "enabled": false
            }}
        }))
        .expect("serialize OpenCode config"),
    )
    .expect("write OpenCode config");

    let aggregate = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--json",
            "doctor",
        ])
        .output()
        .expect("run aggregate doctor");
    assert!(
        aggregate.status.success(),
        "doctor stderr: {}",
        String::from_utf8_lossy(&aggregate.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&aggregate.stdout).expect("aggregate doctor report");
    assert_eq!(report["integration"]["registration_health"], "disabled");
    assert_eq!(report["integration"]["registrations"][0]["enabled"], false);
    assert_eq!(
        report["integration"]["repair_command"],
        "leantoken setup --refresh --yes"
    );

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--json",
            "doctor",
            "--client",
            "opencode",
        ])
        .output()
        .expect("run configured doctor");
    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured doctor error");
    assert_eq!(error["category"], "doctor_failure");
    assert_eq!(error["stage"], "registration");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("disabled"))
    );
}

#[cfg(unix)]
pub(super) fn configured_doctor_rejects_a_registration_changed_during_the_probe() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().expect("temporary home");
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn ready() {}\n").expect("write fixture");
    let database = root.path().join("index.sqlite");
    let config = home.path().join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("create config parent");
    let launcher = home.path().join("mutating-leantoken");
    std::fs::write(
        &launcher,
        "#!/bin/sh\nprintf '\\n# changed during doctor probe\\n' >> \"$LEANTOKEN_TEST_CONFIG\"\nexec \"$LEANTOKEN_TEST_BINARY\" \"$@\"\n",
    )
    .expect("write launcher shim");
    let mut permissions = std::fs::metadata(&launcher)
        .expect("launcher metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&launcher, permissions).expect("make launcher executable");
    let launcher_toml = serde_json::to_string(launcher.to_str().expect("launcher UTF-8"))
        .expect("quote launcher path");
    std::fs::write(
        &config,
        format!("[mcp_servers.leantoken]\ncommand = {launcher_toml}\nargs = [\"mcp\"]\n"),
    )
    .expect("write Codex config");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LEANTOKEN_TEST_CONFIG", &config)
        .env(
            "LEANTOKEN_TEST_BINARY",
            assert_cmd::cargo::cargo_bin!("leantoken"),
        )
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "doctor",
            "--client",
            "codex",
        ])
        .output()
        .expect("run configured doctor");

    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured doctor error");
    assert_eq!(error["category"], "doctor_failure");
    assert_eq!(error["stage"], "registration");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("changed while")),
        "{error}"
    );
}
