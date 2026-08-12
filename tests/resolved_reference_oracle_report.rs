use serde_json::Value;

const MANIFEST: &[u8] = include_bytes!("../benchmarks/resolved_reference_oracle_v1.json");
const SOURCE: &[u8] =
    include_bytes!("../benchmarks/fixtures/resolved_reference_oracle/python_api_migration.py");
const REPORT: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1.json");
const RESOURCE: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1-resource.json");

fn checkout_independent_hash(bytes: &[u8]) -> String {
    let normalized = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    blake3::hash(normalized.as_bytes()).to_hex().to_string()
}

#[test]
fn resolved_reference_artifacts_preserve_declared_bindings() {
    let report: Value = serde_json::from_slice(REPORT).expect("oracle report");
    let resource: Value = serde_json::from_slice(RESOURCE).expect("resource receipt");

    assert_eq!(
        report["manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
    assert_eq!(
        report["source"]["blake3"],
        checkout_independent_hash(SOURCE)
    );
    assert_eq!(
        resource["manifest_blake3"],
        checkout_independent_hash(MANIFEST)
    );
    assert_eq!(resource["source_blake3"], checkout_independent_hash(SOURCE));
    assert_eq!(resource["report_blake3"], checkout_independent_hash(REPORT));
}

#[test]
fn resolved_reference_evidence_preserves_redaction() {
    let _: Value = serde_json::from_slice(REPORT).expect("oracle report");
    let _: Value = serde_json::from_slice(RESOURCE).expect("resource receipt");
    let report = std::str::from_utf8(REPORT).expect("UTF-8 report");
    let resource = std::str::from_utf8(RESOURCE).expect("UTF-8 resource receipt");
    for forbidden in ["/home/", "/tmp/", "droid.resume"] {
        assert!(!report.contains(forbidden), "report leaked {forbidden}");
        assert!(!resource.contains(forbidden), "receipt leaked {forbidden}");
    }
}
