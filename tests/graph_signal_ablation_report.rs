use serde_json::Value;

const REPORT: &str = include_str!("../benchmarks/reports/graph-signal-ablation-v1.json");

fn object_has_forbidden_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            matches!(
                key.as_str(),
                "prompt" | "content" | "raw_target" | "command" | "stdout" | "stderr"
            ) || object_has_forbidden_key(child)
        }),
        Value::Array(values) => values.iter().any(object_has_forbidden_key),
        _ => false,
    }
}

#[test]
fn graph_signal_report_preserves_redaction() {
    let report: Value = serde_json::from_str(REPORT).expect("valid report");
    assert!(!object_has_forbidden_key(&report));
    for forbidden in ["/home/", "/tmp/", "target/phase1", "droid.resume"] {
        assert!(!REPORT.contains(forbidden), "report leaked {forbidden}");
    }
}
