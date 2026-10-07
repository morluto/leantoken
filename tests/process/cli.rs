use clap::Parser;

use super::support::{
    Command, EXPECTED_INDEX_CONTENT_VERSION, assert_cli_parse_error, database_state,
    leantoken_program_name, run, run_error,
};

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
    assert_eq!(
        status["index_content_version"],
        EXPECTED_INDEX_CONTENT_VERSION
    );
    assert_eq!(
        status["indexed_source_bytes"],
        "pub fn answer() -> u8 { 42 }\n".len()
    );
    assert!(
        status["index_storage_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(
        status["index_amplification_ratio"]
            .as_f64()
            .is_some_and(|ratio| ratio > 1.0)
    );

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
        search["meta"]["source_tokens"]
            .as_u64()
            .is_some_and(|value| value <= 100)
    );

    let savings = run(root.path(), &database, &["savings"]);
    assert_eq!(savings["response_accounting"]["tracked_requests"], 1);
    let search_accounting = savings["response_accounting"]["by_operation"]
        .as_array()
        .and_then(|operations| {
            operations
                .iter()
                .find(|operation| operation["operation"] == "search")
        })
        .expect("search accounting");
    assert_eq!(search_accounting["tracked_requests"], 1);
    assert!(
        savings["response_accounting"]["estimated_net_tokens_saved"]
            .as_i64()
            .is_some()
    );
    assert_eq!(savings["observed_task_savings"]["status"], "unavailable");
    assert_eq!(
        savings["observed_task_savings"]["unknown_relevance_responses"],
        1
    );
    assert!(savings["observed_task_savings"]["retry_calls"].is_null());
    assert!(savings["observed_task_savings"]["superseded_calls"].is_null());
    assert_eq!(savings["window"], "lifetime");
    let snapshot = savings["snapshot"]
        .as_str()
        .expect("opaque savings snapshot")
        .to_owned();
    run(
        root.path(),
        &database,
        &["search", "answer", "--mode", "identifier"],
    );
    let delta = run(
        root.path(),
        &database,
        &["savings", "--snapshot", &snapshot],
    );
    assert_eq!(delta["window"], "delta");
    assert_eq!(delta["response_accounting"]["tracked_requests"], 1);
    assert_eq!(delta["observations"]["request_classification"]["useful"], 1);

    let response_limit = search["meta"]["total_response_tokens"]
        .as_u64()
        .expect("exact full response token count")
        .checked_sub(1)
        .expect("non-empty full response token count");
    let response_limit_arg = response_limit.to_string();
    let rejected = run_error(
        root.path(),
        &database,
        &[
            "search",
            "answer",
            "--mode",
            "identifier",
            "--max-tokens",
            "100",
            "--max-response-tokens",
            &response_limit_arg,
        ],
    );
    assert_eq!(rejected["category"], "request_limit_exceeded");
    assert_eq!(rejected["field"], "max_response_tokens");
    assert_eq!(rejected["provided_max_response_tokens"], response_limit);
    let retry_limit = rejected["retry_with_at_least"]
        .as_u64()
        .expect("exact retry token limit");
    assert_eq!(rejected["minimum_required_response_tokens"], retry_limit);

    let retry_limit_arg = retry_limit.to_string();
    let bounded = run(
        root.path(),
        &database,
        &[
            "search",
            "answer",
            "--mode",
            "identifier",
            "--max-tokens",
            "100",
            "--max-response-tokens",
            &retry_limit_arg,
        ],
    );
    assert!(
        bounded["meta"]["total_response_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens <= retry_limit),
        "CLI response exceeded its requested retry limit: {bounded}"
    );
}

#[test]
fn cli_search_compact_and_coordinate_projections_are_source_free() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("lib.rs"),
        "pub fn projection_target() {}\npub fn caller() { projection_target(); }\n",
    )
    .expect("write fixture");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    let compact = run(
        root.path(),
        &database,
        &[
            "search",
            "projection_target",
            "--mode",
            "identifier",
            "--projection",
            "compact",
        ],
    );
    assert_eq!(compact["meta"]["source_tokens"], 0);
    assert!(compact["hits"][0].get("excerpt").is_none());
    assert!(compact["hits"][0].get("score").is_none());
    assert!(compact["hits"][0].get("content_hash").is_none());

    let coordinates = run(
        root.path(),
        &database,
        &[
            "search",
            "projection_target",
            "--mode",
            "text",
            "--all-occurrences",
            "--projection",
            "coordinates",
        ],
    );
    assert_eq!(coordinates["coordinates_only"], true);
    assert_eq!(coordinates["occurrences_total"], 2);
    assert!(coordinates["groups"][0].get("excerpt").is_none());
    assert!(coordinates["groups"][0].get("content_hash").is_none());
}

#[test]
fn cli_scoped_index_omits_dependencies_and_discloses_the_boundary() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::create_dir(root.path().join("third_party")).expect("dependency directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn selected_scope_target() {}\n",
    )
    .expect("write selected fixture");
    std::fs::write(
        root.path().join("third_party/lib.rs"),
        "pub fn dependency_scope_target() {}\n",
    )
    .expect("write dependency fixture");
    let database = root.path().join("scoped.sqlite");
    let scope_args = [
        "--index-include",
        "src/**",
        "--index-exclude",
        "third_party/**",
    ];

    let index = run(
        root.path(),
        &database,
        &[
            scope_args[0],
            scope_args[1],
            scope_args[2],
            scope_args[3],
            "index",
        ],
    );
    assert_eq!(index["files_seen"], 1);
    assert_eq!(index["files_indexed"], 1);
    assert_eq!(index["index_scope"], "scoped");
    assert_eq!(index["index_include_paths"], serde_json::json!(["src/**"]));
    assert_eq!(
        index["index_exclude_paths"],
        serde_json::json!(["third_party/**"])
    );
    assert_eq!(index["index_scope_digest"].as_str().map(str::len), Some(16));

    let status = run(
        root.path(),
        &database,
        &[
            scope_args[0],
            scope_args[1],
            scope_args[2],
            scope_args[3],
            "status",
        ],
    );
    assert_eq!(status["index_scope"], "scoped");
    assert_eq!(status["index_include_paths"], serde_json::json!(["src/**"]));
    assert_eq!(
        status["index_exclude_paths"],
        serde_json::json!(["third_party/**"])
    );
    assert_eq!(status["file_count"], 1);

    let adopted_status = run(root.path(), &database, &["status"]);
    assert_eq!(adopted_status["index_scope"], "scoped");
    assert_eq!(
        adopted_status["index_include_paths"],
        serde_json::json!(["src/**"])
    );
    assert_eq!(adopted_status["file_count"], 1);

    let adopted_search = run(
        root.path(),
        &database,
        &["search", "selected_scope_target", "--mode", "identifier"],
    );
    assert_eq!(adopted_search["hits"][0]["path"], "src/lib.rs");
    assert_eq!(adopted_search["meta"]["index_scope"], "scoped");

    let adopted_context = run(
        root.path(),
        &database,
        &[
            "context",
            "--task",
            "Find the selected scope target",
            "--budget",
            "200",
        ],
    );
    assert_eq!(adopted_context["meta"]["index_scope"], "scoped");
    assert_eq!(
        adopted_context["meta"]["index_scope_digest"],
        adopted_status["index_scope_digest"]
    );

    let absent = run(
        root.path(),
        &database,
        &[
            scope_args[0],
            scope_args[1],
            scope_args[2],
            scope_args[3],
            "search",
            "dependency_scope_target",
            "--mode",
            "identifier",
        ],
    );
    assert!(absent["hits"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(absent["meta"]["index_scope"], "scoped");
    assert_eq!(
        absent["meta"]["index_scope_digest"],
        status["index_scope_digest"]
    );

    let mismatch = run_error(
        root.path(),
        &database,
        &[
            "--index-include",
            "third_party/**",
            "search",
            "dependency_scope_target",
            "--mode",
            "identifier",
        ],
    );
    assert_eq!(mismatch["category"], "index_scope_mismatch");

    let connection = rusqlite::Connection::open(&database).expect("open indexed database");
    connection
        .execute(
            "UPDATE meta SET index_scope_includes = '', index_scope_excludes = ''",
            [],
        )
        .expect("remove persisted scope metadata");
    drop(connection);
    let missing_scope = run_error(root.path(), &database, &["status"]);
    assert_eq!(missing_scope["category"], "repository_configuration");
    assert!(
        missing_scope["error"]
            .as_str()
            .is_some_and(|message| message.contains("no persisted index scope"))
    );
}

#[test]
fn cli_retrieval_reconciles_live_changes_unless_snapshot_consistency_is_requested() {
    let root = tempfile::tempdir().expect("temporary repository");
    let source = root.path().join("lib.rs");
    std::fs::write(&source, "pub fn answer() -> u8 { 41 }\n").expect("write fixture");
    let database = root.path().join("index.sqlite");

    run(root.path(), &database, &["index"]);
    std::fs::write(&source, "pub fn answer() -> u8 { 43 }\n").expect("edit fixture");

    let reconciled = run(root.path(), &database, &["search", "43", "--mode", "text"]);
    assert_eq!(reconciled["hits"][0]["path"], "lib.rs");
    assert_eq!(reconciled["meta"]["repository_generation"], 2);

    std::fs::write(&source, "pub fn answer() -> u8 { 47 }\n").expect("edit fixture again");
    let snapshot = run(
        root.path(),
        &database,
        &[
            "search",
            "43",
            "--mode",
            "text",
            "--consistency",
            "indexed_generation",
        ],
    );
    assert_eq!(snapshot["hits"][0]["path"], "lib.rs");
    assert_eq!(snapshot["meta"]["repository_generation"], 2);

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["working_tree_checked"], false);
}

#[test]
fn cli_savings_renders_a_color_aware_human_table() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write fixture");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);
    run(
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

    let command = || {
        let mut command = Command::cargo_bin("leantoken").expect("binary");
        command.args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "savings",
        ]);
        command
    };

    let plain = command()
        .env("NO_COLOR", "1")
        .output()
        .expect("plain savings report");
    assert!(plain.status.success());
    let plain = String::from_utf8(plain.stdout).expect("plain UTF-8");
    assert!(plain.starts_with("LeanToken Retrieval Accounting\n==============================\n"));
    assert!(plain.contains("Retrieval compression (not task savings)"));
    assert!(plain.contains("Observed task savings"));
    assert!(plain.contains("no task-savings percentage is reported"));
    assert!(plain.contains("Unknown relevance"));
    assert!(plain.contains("Retry calls: unknown  |  superseded calls: unknown"));
    assert!(plain.contains("response tokens"));
    assert!(plain.contains("Persisted observations"));
    assert!(plain.contains("Protocol classes:"));
    assert!(plain.contains("Unobserved task outcomes"));
    assert!(plain.contains("Operation"));
    assert!(plain.contains("Search"));
    assert!(plain.contains("represented-source response delta"));
    assert!(plain.contains("Window: lifetime"));
    assert!(plain.contains("Snapshot: lts1."));
    assert!(!plain.contains("\x1b["));

    let colored = command()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .expect("colored savings report");
    assert!(colored.status.success());
    assert!(
        String::from_utf8(colored.stdout)
            .expect("colored UTF-8")
            .contains("\x1b[1;36mLeanToken Retrieval Accounting\x1b[0m")
    );

    let no_color = command()
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("NO_COLOR savings report");
    assert!(no_color.status.success());
    assert!(
        !String::from_utf8(no_color.stdout)
            .expect("NO_COLOR UTF-8")
            .contains("\x1b[")
    );
}

#[test]
fn cli_index_explains_skipped_binary_files_without_returning_paths() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("write text fixture");
    let binary_path = root.path().join("secret-binary.rs");
    std::fs::write(&binary_path, b"\0binary").expect("write binary fixture");
    let database = root.path().join("index.sqlite");

    let response = run(root.path(), &database, &["index"]);

    assert_eq!(response["files_seen"], 2);
    assert_eq!(response["files_indexed"], 1);
    assert_eq!(response["files_skipped"], 1);
    assert_eq!(
        response["skip_reasons"],
        serde_json::json!({
            "binary": 1,
            "oversized_during_read": 0,
            "failed": 0
        })
    );
    assert_eq!(response["warnings"], serde_json::json!([]));
    assert!(!response.to_string().contains("secret-binary.rs"));
}

#[test]
fn cli_files_tree_treats_dot_as_the_repository_root() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir(root.path().join("src")).expect("src directory");
    std::fs::write(root.path().join("README.md"), "fixture\n").expect("readme");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .expect("source");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    let omitted = run(
        root.path(),
        &database,
        &["files", "tree", "--depth", "2", "--max-results", "2"],
    );
    let dotted = run(
        root.path(),
        &database,
        &[
            "files",
            "tree",
            "--path",
            ".",
            "--depth",
            "2",
            "--max-results",
            "2",
        ],
    );

    assert_eq!(dotted["entries"], omitted["entries"]);
    for key in [
        "repository_id",
        "repository_generation",
        "index_scope",
        "next_cursor",
    ] {
        assert_eq!(dotted["meta"][key], omitted["meta"][key], "meta.{key}");
    }
}

#[test]
fn cold_cli_status_and_retrieval_explain_index_readiness() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn pending() {}\n").expect("source");
    let database = root.path().join("index.sqlite");

    let status = run(root.path(), &database, &["status"]);
    assert_eq!(status["repository_generation"], 0);
    assert_eq!(status["index_state"], "uninitialized");
    assert_eq!(status["freshness"], "current");

    let guidance = "repository index is not ready; run `leantoken index` for direct CLI use \
        or `leantoken doctor` to verify MCP readiness";
    let human = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "files",
            "tree",
        ])
        .output()
        .expect("run human retrieval");
    assert!(!human.status.success());
    assert_eq!(
        String::from_utf8(human.stderr)
            .expect("UTF-8 stderr")
            .trim(),
        format!("Error: {guidance}")
    );

    let json = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--json",
            "files",
            "tree",
        ])
        .output()
        .expect("run JSON retrieval");
    assert!(!json.status.success());
    let error: serde_json::Value = serde_json::from_slice(&json.stderr).expect("structured error");
    assert_eq!(
        error,
        serde_json::json!({
            "error": guidance,
            "category": "index_not_ready"
        })
    );
}

#[test]
fn cli_outline_and_context_emit_workflow_handoff_contracts() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::create_dir_all(root.path().join("tests")).expect("test directory");
    std::fs::create_dir_all(root.path().join("docs")).expect("docs directory");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn workflow_target() -> bool { true }\n",
    )
    .expect("write source");
    std::fs::write(
        root.path().join("tests/lib.rs"),
        "#[test]\nfn workflow_target_regression() { assert!(true); }\n",
    )
    .expect("write test");
    std::fs::write(
        root.path().join("AGENTS.md"),
        "# Contribution rules\nRun focused tests before changing behavior.\n",
    )
    .expect("write repository guidance");
    std::fs::write(
        root.path().join("docs/development.md"),
        "# Development\nValidate changes with the focused test suite.\n",
    )
    .expect("write development guidance");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    let outline = run(
        root.path(),
        &database,
        &["outline", "src/lib.rs", "--projection", "signatures"],
    );
    assert_eq!(outline["files"][0]["path"], "src/lib.rs");
    assert_eq!(outline["returned_symbols"], 1);
    assert_eq!(
        outline["files"][0]["signatures"][0]["name"],
        "workflow_target"
    );
    assert!(outline["files"][0].get("symbols").is_none());

    let context = run(
        root.path(),
        &database,
        &[
            "context",
            "--task",
            "Prepare a contribution for workflow_target",
            "--workflow",
            "contribution",
            "--evidence-symbol",
            "workflow_target",
            "--evidence-path",
            "src/lib.rs",
            "--required-evidence",
            r#"{"path":"src/lib.rs","queries":["workflow_target"]}"#,
            "--test-intent",
            "run workflow_target_regression",
            "--changed-path",
            "src/lib.rs",
            "--response-profile",
            "balanced",
            "--budget",
            "1000",
            "--handoff",
            "--handoff-summary",
            "Review the workflow_target change",
        ],
    );
    assert_eq!(context["workflow"], "contribution");
    assert_eq!(context["effective_response_profile"], "balanced");
    assert_eq!(
        context["coverage"]["required_evidence"][0]["path"],
        "src/lib.rs"
    );
    assert_eq!(
        context["coverage"]["required_evidence"][0]["satisfied"],
        true
    );
    assert_eq!(
        context["coverage"]["required_evidence"][0]["matched_queries"][0],
        "workflow_target"
    );
    assert!(
        context["workflow_receipt"].is_object(),
        "CLI contribution evidence should produce a workflow receipt: {context}"
    );
    let manifest = &context["handoff_manifest"];
    assert_eq!(manifest["summary"], "Review the workflow_target change");
    assert_eq!(
        manifest["repository_generation"],
        context["meta"]["repository_generation"]
    );
    assert!(
        manifest["evidence"]
            .as_array()
            .is_some_and(|evidence| !evidence.is_empty()),
        "handoff should carry selected evidence coordinates: {context}"
    );
}

#[test]
fn cli_json_query_numeric_summary_and_diff_cover_live_files() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(
        root.path().join("before.json"),
        r#"{"service":{"version":1,"timeout":30},"samples":[2,4,6]}"#,
    )
    .expect("write base JSON");
    std::fs::write(
        root.path().join("after.json"),
        r#"{"service":{"version":2,"timeout":30},"samples":[2,4,6]}"#,
    )
    .expect("write head JSON");
    std::fs::write(root.path().join("broken.json"), "{broken").expect("write malformed JSON");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    let query = run(
        root.path(),
        &database,
        &[
            "json",
            "query",
            "before.json",
            "--pointer",
            "/service/version",
        ],
    );
    assert_eq!(query["kind"], "query");
    assert_eq!(query["value"], 1);
    assert_eq!(query["sources"][0]["path"], "before.json");

    let summary = run(
        root.path(),
        &database,
        &[
            "json",
            "numeric-summary",
            "before.json",
            "--pointer",
            "/samples",
        ],
    );
    assert_eq!(summary["kind"], "numeric_summary");
    assert_eq!(summary["numeric_summary"]["count"], 3);
    assert_eq!(summary["numeric_summary"]["min"], 2.0);
    assert_eq!(summary["numeric_summary"]["median"], 4.0);
    assert_eq!(summary["numeric_summary"]["max"], 6.0);

    let diff = run(
        root.path(),
        &database,
        &[
            "json",
            "diff-fields",
            "before.json",
            "after.json",
            "--pointer",
            "/service/version",
            "--pointer",
            "/service/timeout",
        ],
    );
    assert_eq!(diff["kind"], "diff_fields");
    assert_eq!(diff["differences"].as_array().map(Vec::len), Some(2));
    assert_eq!(diff["differences"][0]["before"], 1);
    assert_eq!(diff["differences"][0]["after"], 2);
    assert_eq!(diff["differences"][0]["changed"], true);
    assert_eq!(diff["differences"][1]["changed"], false);

    let malformed = run_error(root.path(), &database, &["json", "query", "broken.json"]);
    assert_eq!(malformed["category"], "invalid_json");
    assert_eq!(malformed["field"], "path");
    assert!(
        malformed["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );

    let invalid_selector = run_error(
        root.path(),
        &database,
        &["json", "query", "before.json", "--jmespath", "service["],
    );
    assert_eq!(invalid_selector["category"], "invalid_json_selector");
    assert_eq!(invalid_selector["stage"], "compile");
    assert_eq!(invalid_selector["field"], "JMESPath expression");
    assert!(
        invalid_selector["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
}

#[test]
fn cli_history_reads_diffs_and_lists_commits_for_a_symbol() {
    let root = tempfile::tempdir().expect("temporary repository");
    let repository = root.path();
    leantoken_test_support::GitFixture::init(repository);
    std::fs::write(
        repository.join("lib.rs"),
        "pub fn history_target() -> u8 { 1 }\n",
    )
    .expect("write initial source");
    let base = leantoken_test_support::GitFixture::commit_all(repository, "add history target");

    std::fs::write(
        repository.join("lib.rs"),
        "pub fn history_target() -> u8 { 2 }\n",
    )
    .expect("change source");
    let head = leantoken_test_support::GitFixture::commit_all(repository, "change history target");
    let database = repository.join("index.sqlite");
    run(repository, &database, &["index"]);

    let historical = run(
        repository,
        &database,
        &["history", "read-symbol", "lib.rs", "history_target", &base],
    );
    assert_eq!(historical["kind"], "read_symbol");
    assert!(
        historical["symbol"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("{ 1 }"))
    );

    let diff = run(
        repository,
        &database,
        &[
            "history",
            "diff-symbol",
            "lib.rs",
            "history_target",
            &base,
            &head,
        ],
    );
    assert_eq!(diff["kind"], "diff_symbol");
    assert_eq!(diff["before"]["name"], "history_target");
    assert_eq!(diff["after"]["name"], "history_target");
    assert!(diff["before"].get("content").is_none());
    assert!(diff["after"].get("content").is_none());
    let patch = diff["diff"].as_str().expect("unified symbol diff");
    assert!(patch.contains("-pub fn history_target() -> u8 { 1 }"));
    assert!(patch.contains("+pub fn history_target() -> u8 { 2 }"));

    let targets = r#"[{"path":"lib.rs","symbol":"history_target"}]"#;
    let batch = run(
        repository,
        &database,
        &["history", "diff-symbols", targets, &base, &head],
    );
    assert_eq!(batch["kind"], "diff_symbols");
    assert_eq!(batch["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(batch["results"][0]["status"], "modified");
    assert_eq!(batch["results"][0]["before"]["name"], "history_target");
    assert_eq!(batch["results"][0]["after"]["name"], "history_target");
    let batch_patch = batch["results"][0]["diff"]
        .as_str()
        .expect("batched symbol diff");
    assert!(batch_patch.contains("-pub fn history_target() -> u8 { 1 }"));
    assert!(batch_patch.contains("+pub fn history_target() -> u8 { 2 }"));

    let log = run(
        repository,
        &database,
        &["history", "symbol-log", "lib.rs", "history_target"],
    );
    assert_eq!(log["kind"], "symbol_log");
    assert_eq!(log["commits"].as_array().map(Vec::len), Some(2));
    assert!(log["commits"].to_string().contains("change history target"));

    let invalid_json = run_error(
        repository,
        &database,
        &["history", "diff-symbols", "[", &base, &head],
    );
    assert_eq!(invalid_json["category"], "invalid_input");
    assert!(
        invalid_json["error"]
            .as_str()
            .is_some_and(|message| message.contains("targets must be a JSON array"))
    );

    let invalid_shape = run_error(
        repository,
        &database,
        &["history", "diff-symbols", "[{}]", &base, &head],
    );
    assert_eq!(invalid_shape["category"], "invalid_input");
    assert!(
        invalid_shape["error"]
            .as_str()
            .is_some_and(|message| message.contains("targets must be a JSON array"))
    );
}

#[test]
fn cli_json_errors_expose_stable_safe_metadata() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("lib.rs"), "fn indexed() {}\n").expect("source");
    let database = root.path().join("index.sqlite");
    run(root.path(), &database, &["index"]);

    assert_eq!(
        run_error(root.path(), &database, &["files", "find"]),
        serde_json::json!({
            "error": "invalid query: is required for find",
            "category": "invalid_input",
            "field": "query"
        })
    );
    assert_eq!(
        run_error(
            root.path(),
            &database,
            &["files", "tree", "--max-results", "101"],
        ),
        serde_json::json!({
            "error": "max_results exceeds its configured limit: requested 101, limit 100",
            "category": "request_limit_exceeded",
            "field": "max_results",
            "requested": 101,
            "limit": 100
        })
    );
    assert_eq!(
        run_error(root.path(), &database, &["read", "missing.rs"]),
        serde_json::json!({
            "error": "path is not indexed: missing.rs",
            "category": "not_indexed"
        })
    );
    assert_eq!(
        run_error(
            root.path(),
            &database,
            &["files", "tree", "--cursor", "malformed"],
        ),
        serde_json::json!({
            "error": "stale cursor",
            "category": "stale_cursor"
        })
    );

    let oversized_pattern = "x".repeat(4_097);
    let error = run_error(
        root.path(),
        &database,
        &["files", "glob", "--pattern", &oversized_pattern],
    );
    assert_eq!(error["category"], "input_too_long");
    assert_eq!(error["field"], "pattern");
    assert_eq!(error["limit"], 4_096);

    let incompatible_context = run_error(
        root.path(),
        &database,
        &[
            "context",
            "--task",
            "change indexed",
            "--strict-focus-paths",
            "--plan-only",
            "--handoff",
        ],
    );
    assert_eq!(incompatible_context["category"], "invalid_input");
    assert_eq!(
        incompatible_context["violations"][0]["field"],
        "focus paths"
    );
    assert_eq!(incompatible_context["violations"][1]["field"], "plan_only");

    let database_directory = root.path().join("database-directory");
    std::fs::create_dir(&database_directory).expect("database directory");
    let internal = run_error(root.path(), &database_directory, &["status"]);
    assert_eq!(internal["category"], "internal_error");
    assert!(
        internal["error"]
            .as_str()
            .is_some_and(|message| message.starts_with("SQLite error:"))
    );
    assert_eq!(internal.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn cli_regex_chunk_limit_reports_path_and_remediation() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("large.txt"), "x\n".repeat(20_560))
        .expect("write bounded large-file fixture");
    let database = root.path().join("index.sqlite");
    run(
        root.path(),
        &database,
        &["--tokenizer", "estimate", "index"],
    );

    let error = run_error(root.path(), &database, &["search", ".", "--mode", "regex"]);
    assert_eq!(error["category"], "request_limit_exceeded");
    assert_eq!(error["reason"], "regex_chunks_per_file");
    assert_eq!(error["blocking_path"], "large.txt");
    assert_eq!(error["requested"], 257);
    assert_eq!(error["limit"], 256);
    assert!(error["error"].as_str().is_some_and(|message| {
        message.contains("exclude or narrow paths that include unusually large files")
    }));
}

#[test]
fn cli_json_parse_errors_are_structured_without_changing_clap_help() {
    assert_cli_parse_error(&["files", "tree", "--max-results", "nope", "--json"]);
    assert_cli_parse_error(&["search", "answer", "--max-response-tokens", "0", "--json"]);
    assert_cli_parse_error(&[
        "context",
        "--task",
        "review",
        "--required-evidence",
        "{",
        "--json",
    ]);
    assert_cli_parse_error(&["--json", "--unknown"]);

    let human_arguments = ["files", "tree", "--max-results", "nope"];
    let expected = leantoken::cli::Cli::try_parse_from(
        std::iter::once(leantoken_program_name())
            .chain(human_arguments.into_iter().map(std::ffi::OsString::from)),
    )
    .expect_err("invalid numeric argument")
    .to_string();
    let human = Command::cargo_bin("leantoken")
        .expect("binary")
        .args(human_arguments)
        .output()
        .expect("run human parse failure");
    assert_eq!(human.status.code(), Some(2));
    assert!(human.stdout.is_empty());
    assert_eq!(human.stderr, expected.as_bytes());

    let help = Command::cargo_bin("leantoken")
        .expect("binary")
        .args(["--json", "--help"])
        .output()
        .expect("run JSON help");
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: leantoken"));
}

#[test]
fn cli_index_limit_error_is_structured_and_does_not_publish_partial_files() {
    let root = tempfile::tempdir().expect("temporary repository");
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").expect("a");
    std::fs::write(root.path().join("b.rs"), "fn b() {}\n").expect("b");
    let database = root.path().join("index.sqlite");

    let output = Command::cargo_bin("leantoken")
        .expect("binary")
        .args([
            "--root",
            root.path().to_str().expect("root UTF-8"),
            "--database",
            database.to_str().expect("database UTF-8"),
            "--max-files",
            "1",
            "--json",
            "index",
        ])
        .output()
        .expect("run index");

    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("structured error");
    assert_eq!(
        error["error"],
        "index source files limit exceeded: observed 2, limit 1"
    );
    assert_eq!(error["category"], "repository_index_limit");
    assert_eq!(database_state(&database).map(|state| state.0), Some(0));
    assert_eq!(database_state(&database).map(|state| state.1), Some(0));
}
