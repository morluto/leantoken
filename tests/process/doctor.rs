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
    assert_eq!(report["tools"].as_array().map(Vec::len), Some(9));
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
    assert!(stdout.contains("Tool catalog: 9 MCP tools"));
    assert!(stdout.contains("leantoken.context first"));
}
