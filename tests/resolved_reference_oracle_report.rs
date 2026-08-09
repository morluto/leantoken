use serde_json::Value;

const REPORT: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1.json");
const RESOURCE: &[u8] =
    include_bytes!("../benchmarks/reports/resolved-reference-oracle-python-v1-resource.json");

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
