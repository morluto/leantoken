use super::*;

#[test]
fn cli_json_detection_ignores_values_after_the_argument_separator() {
    let arguments = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

    assert!(cli_json_requested(&arguments(&[
        "leantoken",
        "files",
        "tree",
        "--json"
    ])));
    assert!(!cli_json_requested(&arguments(&[
        "leantoken",
        "search",
        "--",
        "--json"
    ])));
}

#[test]
fn mcp_background_indexing_defaults_to_one_worker_but_preserves_explicit_limit() {
    assert_eq!(mcp_index_worker_limit(4, false), 1);
    assert_eq!(mcp_index_worker_limit(3, true), 3);
}
