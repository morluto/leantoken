use serde_json::Value;

const GRAPH_REPORT: &[u8] = include_bytes!("../benchmarks/reports/graph-signal-ablation-v1.json");
const GRAPH_MANIFEST: &[u8] = include_bytes!("../benchmarks/graph_signal_ablation_v1.json");
const GRAPH_SOURCE_MANIFEST: &[u8] = include_bytes!("../benchmarks/representative.json");
const TRAJECTORY_REPORT: &[u8] =
    include_bytes!("../benchmarks/reports/model-ab-trajectory-v1.json");
const TRAJECTORY_CLASSIFIER: &[u8] = include_bytes!("../examples/model_ab_trajectory.rs");
const TRAJECTORY_MANIFEST: &[u8] = include_bytes!("../benchmarks/model_ab_trajectory_v1.json");
const ORACLE_MANIFEST: &[u8] = include_bytes!("../benchmarks/resolved_reference_oracle_v1.json");
const ORACLE_SOURCE: &[u8] =
    include_bytes!("../benchmarks/fixtures/resolved_reference_oracle/python_api_migration.py");
const ORACLE_REPORT: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1.json");
const ORACLE_RESOURCE: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1-resource.json");

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
fn frozen_report_bindings_match_committed_sources() {
    let graph: Value = serde_json::from_slice(GRAPH_REPORT).expect("graph report");
    assert_eq!(
        graph["manifest_blake3"],
        checkout_independent_hash(GRAPH_MANIFEST)
    );
    assert_eq!(
        graph["source_manifest_blake3"],
        checkout_independent_hash(GRAPH_SOURCE_MANIFEST)
    );

    let trajectory: Value = serde_json::from_slice(TRAJECTORY_REPORT).expect("trajectory report");
    assert_eq!(
        trajectory["source"]["classifier_source_blake3"],
        checkout_independent_hash(TRAJECTORY_CLASSIFIER)
    );
    assert_eq!(
        trajectory["source"]["classifier_manifest_blake3"],
        checkout_independent_hash(TRAJECTORY_MANIFEST)
    );

    let oracle: Value = serde_json::from_slice(ORACLE_REPORT).expect("oracle report");
    let resource: Value = serde_json::from_slice(ORACLE_RESOURCE).expect("oracle resource");
    assert_eq!(
        oracle["manifest_blake3"],
        checkout_independent_hash(ORACLE_MANIFEST)
    );
    assert_eq!(
        oracle["source"]["blake3"],
        checkout_independent_hash(ORACLE_SOURCE)
    );
    assert_eq!(
        resource["manifest_blake3"],
        checkout_independent_hash(ORACLE_MANIFEST)
    );
    assert_eq!(
        resource["source_blake3"],
        checkout_independent_hash(ORACLE_SOURCE)
    );
    assert_eq!(
        resource["report_blake3"],
        checkout_independent_hash(ORACLE_REPORT)
    );
}

#[test]
fn frozen_reports_preserve_redaction() {
    let graph: Value = serde_json::from_slice(GRAPH_REPORT).expect("graph report");
    assert!(!object_has_forbidden_key(&graph));
    let graph_text = std::str::from_utf8(GRAPH_REPORT).expect("UTF-8 graph report");
    for forbidden in ["/home/", "/tmp/", "target/phase1", "droid.resume"] {
        assert!(
            !graph_text.contains(forbidden),
            "graph report leaked {forbidden}"
        );
    }

    let trajectory_text = std::str::from_utf8(TRAJECTORY_REPORT).expect("UTF-8 trajectory report");
    for forbidden in [
        "/home/",
        "aggregated_output",
        "success_command",
        "worktree_patch",
        "\"arguments\"",
        "\"prompt\"",
    ] {
        assert!(
            !trajectory_text.contains(forbidden),
            "trajectory report leaked {forbidden}"
        );
    }

    for (label, bytes) in [
        ("oracle report", ORACLE_REPORT),
        ("oracle resource", ORACLE_RESOURCE),
    ] {
        let text = std::str::from_utf8(bytes).expect("UTF-8 oracle artifact");
        for forbidden in ["/home/", "/tmp/", "droid.resume"] {
            assert!(!text.contains(forbidden), "{label} leaked {forbidden}");
        }
    }
}
