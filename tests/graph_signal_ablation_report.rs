use serde_json::Value;

const REPORT: &str = include_str!("../benchmarks/reports/graph-signal-ablation-v1.json");
const MANIFEST: &[u8] = include_bytes!("../benchmarks/graph_signal_ablation_v1.json");
const SOURCE_MANIFEST: &[u8] = include_bytes!("../benchmarks/representative.json");
const HARNESS: &[u8] = include_bytes!("../examples/graph_signal_ablation.rs");

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
fn graph_signal_report_binds_frozen_inputs_and_no_go_decision() {
    let report: Value = serde_json::from_str(REPORT).expect("valid report");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["experiment_id"], "graph-signal-ablation-v1");
    assert_eq!(
        report["manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
    assert_eq!(
        report["source_manifest_blake3"],
        checkout_independent_hash(SOURCE_MANIFEST)
    );
    assert_eq!(
        report["harness_source_blake3"],
        checkout_independent_hash(HARNESS)
    );
    assert_eq!(
        report["harness_revision"],
        "acf0e240a58030b09f8326858ae1ed300ac6ed58"
    );
    assert_eq!(report["harness_worktree_dirty"], true);
    assert_eq!(report["runs"].as_array().expect("runs").len(), 96);

    let arms = report["arms"].as_array().expect("arms");
    assert_eq!(arms.len(), 4);
    for arm in arms {
        assert_eq!(arm["deterministic_metrics_repeat"], true);
        assert_eq!(arm["deterministic_task_results_repeat"], true);
        let repetitions = arm["per_repetition"].as_array().expect("repetitions");
        assert_eq!(repetitions.len(), 3);
        assert!(
            repetitions
                .iter()
                .all(|totals| totals["additive_violations"] == 0)
        );
    }
    assert_eq!(report["decision"]["issue_outcome"], "no_go");
    assert_eq!(report["decision"]["expose_graph_metadata"], false);
    assert!(
        report["decision"]["retained_ranking_signals"]
            .as_array()
            .expect("retained signals")
            .is_empty()
    );
    assert!(
        report["decision"]["ranking_signal_decisions"]
            .as_array()
            .expect("signal decisions")
            .iter()
            .all(|decision| decision["retain_ranking_signal"] == false)
    );

    let reverse = arms
        .iter()
        .find(|arm| arm["arm"] == "reverse_dependency")
        .expect("reverse arm");
    assert_eq!(
        reverse["per_repetition"][0]["false_positive_signal_candidate_files"],
        15
    );
    assert_eq!(
        reverse["per_repetition"][0]
            ["applicable_signal_tasks_without_relevant_candidate"],
        4
    );
    let caller = arms
        .iter()
        .find(|arm| arm["arm"] == "high_confidence_caller")
        .expect("caller arm");
    assert_eq!(
        caller["per_repetition"][0]["false_positive_signal_candidate_files"],
        127
    );
    assert_eq!(report["graph_index"]["unresolved_import_edges"], 8012);
    assert_eq!(report["graph_index"]["total_database_bytes"], 113127424);
}

#[test]
fn graph_signal_report_preserves_evaluation_only_boundary_and_redaction() {
    let report: Value = serde_json::from_str(REPORT).expect("valid report");
    assert!(!object_has_forbidden_key(&report));
    for forbidden in ["/home/", "/tmp/", "target/phase1", "droid.resume"] {
        assert!(!REPORT.contains(forbidden), "report leaked {forbidden}");
    }

    let context_source = include_str!("../src/services/context.rs");
    assert!(context_source.contains("ContextSignals::PRODUCTION"));
    assert!(context_source.contains("pub async fn context_signal_evaluation"));
    for adapter in [include_str!("../src/mcp.rs"), include_str!("../src/cli.rs")] {
        assert!(!adapter.contains("ContextSignalPolicy"));
        assert!(!adapter.contains("context_signal_evaluation"));
    }
}

#[test]
fn checkout_independent_hash_normalizes_crlf() {
    assert_eq!(
        checkout_independent_hash(b"one\r\ntwo\r\n"),
        checkout_independent_hash(b"one\ntwo\n")
    );
}
