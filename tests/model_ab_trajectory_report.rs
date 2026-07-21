use serde_json::Value;

const REPORT: &[u8] = include_bytes!("../benchmarks/reports/model-ab-trajectory-v1.json");
const CLASSIFIER: &[u8] = include_bytes!("../examples/model_ab_trajectory.rs");
const MANIFEST: &[u8] = include_bytes!("../benchmarks/model_ab_trajectory_v1.json");

fn checkout_independent_hash(bytes: &[u8]) -> String {
    let text = std::str::from_utf8(bytes).expect("UTF-8 checked artifact");
    blake3::hash(text.replace("\r\n", "\n").as_bytes())
        .to_hex()
        .to_string()
}

#[test]
fn test_model_ab_trajectory_report_binds_redacted_classifier_evidence() {
    assert_eq!(
        checkout_independent_hash(b"line\r\n"),
        blake3::hash(b"line\n").to_hex().to_string()
    );
    let report: Value = serde_json::from_slice(REPORT).expect("trajectory report");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "completed_post_hoc_no_go");
    assert_eq!(report["decision"]["result"], "no_go");
    assert_eq!(report["decision"]["change_tool_descriptions"], false);
    assert_eq!(report["decision"]["add_receipt_fields"], false);
    assert_eq!(report["decision"]["add_next_useful_action"], false);
    assert_eq!(report["decision"]["add_server_session_state"], false);
    assert_eq!(report["runs"].as_array().map(Vec::len), Some(36));
    assert_eq!(report["source"]["verified_artifacts"], 55);
    assert_eq!(
        report["source"]["classifier_source_blake3"],
        checkout_independent_hash(CLASSIFIER)
    );
    assert_eq!(
        report["source"]["classifier_manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );

    let text = std::str::from_utf8(REPORT).expect("UTF-8 report");
    for forbidden in [
        "/home/",
        "aggregated_output",
        "success_command",
        "worktree_patch",
        "\"arguments\"",
        "\"prompt\"",
    ] {
        assert!(!text.contains(forbidden), "report leaked {forbidden}");
    }
}
