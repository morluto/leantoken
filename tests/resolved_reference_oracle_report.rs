use serde_json::Value;

const MANIFEST: &[u8] = include_bytes!("../benchmarks/resolved_reference_oracle_v1.json");
const SOURCE: &[u8] = include_bytes!(
    "../benchmarks/fixtures/resolved_reference_oracle/python_api_migration.py"
);
const REPORT: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1.json");
const RESOURCE: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1-resource.json");

fn checkout_independent_hash(bytes: &[u8]) -> String {
    let normalized = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    blake3::hash(normalized.as_bytes()).to_hex().to_string()
}

#[test]
fn resolved_reference_report_binds_exact_oracle_and_no_public_tool_decision() {
    let report: Value = serde_json::from_slice(REPORT).expect("oracle report");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(
        report["manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
    assert_eq!(report["source"]["blake3"], checkout_independent_hash(SOURCE));
    assert_eq!(report["engine"]["evaluation_only"], true);
    assert_eq!(report["engine"]["production_index_rows_loaded"], 0);
    assert_eq!(report["comparison"]["expected_occurrences"], 17);
    assert_eq!(report["comparison"]["observed_occurrences"], 17);
    assert_eq!(report["comparison"]["false_positives"], 0);
    assert_eq!(report["comparison"]["false_negatives"], 0);
    assert_eq!(report["comparison"]["classification_mismatches"], 0);
    assert_eq!(report["comparison"]["passed"], true);
    assert_eq!(report["comparison"]["resolved"]["observed"], 11);
    assert_eq!(report["comparison"]["ambiguous"]["observed"], 1);
    assert_eq!(report["comparison"]["unrelated"]["observed"], 5);
    assert_eq!(report["measurements"]["unique_ast_nodes_collected"], 277);
    assert_eq!(
        report["measurements"]["modeled_post_parse_ast_node_inspection_upper_bound"],
        103_200_000_000_u64
    );
    assert_eq!(
        report["measurements"]["modeled_post_parse_lookup_iteration_upper_bound"],
        2_056_000_000_000_000_u64
    );
    assert!(
        report["measurements"]["modeled_partial_allocation_estimate_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(
        report["measurements"]
            .get("modeled_retained_memory_bound_bytes")
            .is_none()
    );
    assert!(
        report["limitations"]
            .as_array()
            .is_some_and(|limitations| limitations.iter().any(|item| item
                .as_str()
                .is_some_and(|text| text.contains("not a memory bound"))))
    );
    assert_eq!(
        report["decision"]["issue_outcome"],
        "evaluation_complete_no_public_tool"
    );
    assert_eq!(report["decision"]["add_public_impact_analysis_tool"], false);
}

#[test]
fn resolved_reference_resource_receipt_is_descriptive_and_bound_to_report() {
    let resource: Value = serde_json::from_slice(RESOURCE).expect("resource receipt");
    assert_eq!(
        resource["manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
    assert_eq!(resource["source_blake3"], checkout_independent_hash(SOURCE));
    assert_eq!(resource["report_blake3"], checkout_independent_hash(REPORT));
    assert_eq!(resource["measurement"]["exit_status"], 0);
    assert!(
        resource["measurement"]["peak_process_rss_kib"]
            .as_u64()
            .is_some_and(|rss| rss > 0)
    );
    assert_eq!(resource["interpretation"]["oracle_passed"], true);
    assert_eq!(resource["interpretation"]["promotion_gate"], false);
    assert_eq!(
        resource["interpretation"]["resource_regression_claim"],
        false
    );
}

#[test]
fn resolved_reference_evidence_preserves_the_evaluation_only_boundary() {
    let report = std::str::from_utf8(REPORT).expect("UTF-8 report");
    let resource = std::str::from_utf8(RESOURCE).expect("UTF-8 resource receipt");
    for forbidden in ["/home/", "/tmp/", "droid.resume"] {
        assert!(!report.contains(forbidden), "report leaked {forbidden}");
        assert!(!resource.contains(forbidden), "receipt leaked {forbidden}");
    }
    for adapter in [
        include_str!("../src/cli/mod.rs"),
        include_str!("../src/mcp/mod.rs"),
    ] {
        assert!(!adapter.contains("resolved_reference_oracle"));
        assert!(!adapter.contains("impact_analysis"));
    }
}
