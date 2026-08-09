use serde_json::Value;

const REPORT: &[u8] = include_bytes!("../benchmarks/reports/model-ab-trajectory-v1.json");

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
