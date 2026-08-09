use serde_json::Value;

const REPORT: &[u8] = include_bytes!("../benchmarks/reports/model-ab-trajectory-v1.json");
const CLASSIFIER: &[u8] = include_bytes!("../examples/model_ab_trajectory.rs");
const MANIFEST: &[u8] = include_bytes!("../benchmarks/model_ab_trajectory_v1.json");

fn checkout_independent_hash(bytes: &[u8]) -> String {
    let normalized = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    blake3::hash(normalized.as_bytes()).to_hex().to_string()
}

#[test]
fn model_ab_report_preserves_declared_bindings() {
    let report: Value = serde_json::from_slice(REPORT).expect("trajectory report");

    assert_eq!(
        report["source"]["classifier_source_blake3"],
        checkout_independent_hash(CLASSIFIER)
    );
    assert_eq!(
        report["source"]["classifier_manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
}

#[test]
fn model_ab_report_preserves_redaction() {
    let _: Value = serde_json::from_slice(REPORT).expect("trajectory report");
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
