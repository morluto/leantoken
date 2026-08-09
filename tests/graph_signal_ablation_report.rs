use serde_json::Value;

const REPORT: &str = include_str!("../benchmarks/reports/graph-signal-ablation-v1.json");
const MANIFEST: &[u8] = include_bytes!("../benchmarks/graph_signal_ablation_v1.json");
const SOURCE_MANIFEST: &[u8] = include_bytes!("../benchmarks/representative.json");

fn checkout_independent_hash(bytes: &[u8]) -> String {
    let normalized = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    blake3::hash(normalized.as_bytes()).to_hex().to_string()
}

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
fn graph_signal_report_preserves_declared_bindings() {
    let report: Value = serde_json::from_str(REPORT).expect("valid report");

    assert_eq!(
        report["manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
    assert_eq!(
        report["source_manifest_blake3"],
        checkout_independent_hash(SOURCE_MANIFEST)
    );
}

#[test]
fn graph_signal_report_preserves_redaction() {
    let report: Value = serde_json::from_str(REPORT).expect("valid report");
    assert!(!object_has_forbidden_key(&report));
    for forbidden in ["/home/", "/tmp/", "target/phase1", "droid.resume"] {
        assert!(!REPORT.contains(forbidden), "report leaked {forbidden}");
    }
}
