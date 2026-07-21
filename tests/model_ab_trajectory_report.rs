use serde_json::Value;

const REPORT: &[u8] = include_bytes!("../benchmarks/reports/model-ab-trajectory-v1.json");
const CLASSIFIER: &[u8] = include_bytes!("../examples/model_ab_trajectory.rs");
const MANIFEST: &[u8] = include_bytes!("../benchmarks/model_ab_trajectory_v1.json");

#[test]
fn test_model_ab_trajectory_report_binds_redacted_classifier_evidence() {
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
        blake3::hash(CLASSIFIER).to_hex().as_str()
    );
    assert_eq!(
        report["source"]["classifier_manifest_blake3"],
        blake3::hash(MANIFEST).to_hex().as_str()
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
