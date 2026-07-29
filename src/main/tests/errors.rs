    #[test]
    fn cli_error_json_has_exact_safe_metadata() {
        let cases = [
            (
                leantoken::Error::InvalidInput {
                    field: "query",
                    reason: "is required for find",
                },
                serde_json::json!({
                    "error": "invalid query: is required for find",
                    "category": "invalid_input",
                    "field": "query"
                }),
            ),
            (
                leantoken::Error::InvalidInputConstraints(leantoken::InputViolations::new(vec![
                    leantoken::InputViolation {
                        field: "focus paths",
                        reason: "must not be empty when focus path constraints are enabled",
                    },
                    leantoken::InputViolation {
                        field: "plan_only",
                        reason: "cannot be combined with a handoff manifest",
                    },
                ])),
                serde_json::json!({
                    "error": "invalid input constraints: focus paths: must not be empty when focus path constraints are enabled; plan_only: cannot be combined with a handoff manifest",
                    "category": "invalid_input",
                    "violations": [
                        {
                            "field": "focus paths",
                            "reason": "must not be empty when focus path constraints are enabled"
                        },
                        {
                            "field": "plan_only",
                            "reason": "cannot be combined with a handoff manifest"
                        }
                    ]
                }),
            ),
            (
                leantoken::Error::RequestLimitExceeded {
                    field: "max_results",
                    requested: 101,
                    limit: 100,
                },
                serde_json::json!({
                    "error": "max_results exceeds its configured limit: requested 101, limit 100",
                    "category": "request_limit_exceeded",
                    "field": "max_results",
                    "requested": 101,
                    "limit": 100
                }),
            ),
            (
                leantoken::Error::ResponseBudgetExceeded {
                    provided_max_response_tokens: 40,
                    minimum_required_response_tokens: 73,
                    retry_with_at_least: 73,
                    breakdown: leantoken::ResponseBudgetBreakdown {
                        mandatory_response_tokens: 61,
                        source_tokens: 17,
                        protocol_tokens: 20,
                        path_and_metadata_tokens: 24,
                        receipt_reserve_tokens: 12,
                    },
                },
                serde_json::json!({
                    "error": "max_response_tokens is too small: provided 40, minimum required 73; retry with at least 73",
                    "category": "request_limit_exceeded",
                    "field": "max_response_tokens",
                    "requested": 73,
                    "limit": 40,
                    "provided_max_response_tokens": 40,
                    "minimum_required_response_tokens": 73,
                    "retry_with_at_least": 73,
                    "breakdown": {
                        "mandatory_response_tokens": 61,
                        "source_tokens": 17,
                        "protocol_tokens": 20,
                        "path_and_metadata_tokens": 24,
                        "receipt_reserve_tokens": 12
                    }
                }),
            ),
            (
                leantoken::Error::LimitExceeded,
                serde_json::json!({
                    "error": "requested content exceeds the configured limit",
                    "category": "request_limit_exceeded"
                }),
            ),
            (
                leantoken::Error::RetrievalLimitExceeded {
                    kind: leantoken::RetrievalLimitKind::RegexFullScanFiles,
                    observed: 10_001,
                    limit: 10_000,
                },
                serde_json::json!({
                    "error": "retrieval regex_full_scan_files limit exceeded: observed 10001, limit 10000; add a mandatory case-sensitive literal or use a smaller index scope",
                    "category": "request_limit_exceeded",
                    "requested": 10_001,
                    "limit": 10_000,
                    "reason": "regex_full_scan_files"
                }),
            ),
            (
                leantoken::Error::InvalidJson {
                    syntax_category: "syntax",
                    byte_offset: 12,
                    line: 1,
                    column: 13,
                    reason: "trailing comma at line 1 column 13".into(),
                },
                serde_json::json!({
                    "error": "file is not valid JSON (syntax at byte 12, line 1, column 13): trailing comma at line 1 column 13",
                    "category": "invalid_json",
                    "field": "path",
                    "reason": "trailing comma at line 1 column 13",
                    "syntax_category": "syntax",
                    "byte_offset": 12,
                    "line": 1,
                    "column": 13
                }),
            ),
            (
                leantoken::Error::InvalidJsonSelector {
                    stage: "evaluate",
                    offset: 6,
                    line: 1,
                    column: 7,
                    reason: "Runtime error: Argument 0 expects type array, given number".into(),
                },
                serde_json::json!({
                    "error": "JMESPath evaluate failed at offset 6, line 1, column 7: Runtime error: Argument 0 expects type array, given number",
                    "category": "invalid_json_selector",
                    "stage": "evaluate",
                    "field": "JMESPath expression",
                    "reason": "Runtime error: Argument 0 expects type array, given number",
                    "offset": 6,
                    "line": 1,
                    "column": 7
                }),
            ),
            (
                leantoken::Error::InputTooLong {
                    field: "query",
                    max_bytes: 65_536,
                },
                serde_json::json!({
                    "error": "query exceeds 65536 bytes",
                    "category": "input_too_long",
                    "field": "query",
                    "limit": 65_536
                }),
            ),
            (
                leantoken::Error::NotIndexed("missing.rs".into()),
                serde_json::json!({
                    "error": "path is not indexed: missing.rs",
                    "category": "not_indexed"
                }),
            ),
            (
                leantoken::Error::SymbolNotFound {
                    path: "lib.rs".into(),
                    symbol: "missing".into(),
                },
                serde_json::json!({
                    "error": "symbol is not indexed in lib.rs: missing",
                    "category": "symbol_not_found"
                }),
            ),
            (
                leantoken::Error::AmbiguousSymbol {
                    path: "lib.rs".into(),
                    symbol: "run".into(),
                },
                serde_json::json!({
                    "error": "symbol is ambiguous in lib.rs: run",
                    "category": "symbol_ambiguous"
                }),
            ),
            (
                leantoken::Error::HeadingNotFound {
                    path: "README.md".into(),
                    heading: "Installation".into(),
                    occurrence: 2,
                },
                serde_json::json!({
                    "error": "document heading occurrence 2 is not indexed in README.md: Installation",
                    "category": "heading_not_found"
                }),
            ),
            (
                leantoken::Error::IndexNotReady,
                serde_json::json!({
                    "error": "repository index is not ready; run `leantoken index` for direct CLI use or `leantoken doctor` to verify MCP readiness",
                    "category": "index_not_ready"
                }),
            ),
            (
                leantoken::Error::ReconciliationFailed(Arc::new(leantoken::Error::IndexNotReady)),
                serde_json::json!({
                    "error": "repository index is not ready; run `leantoken index` for direct CLI use or `leantoken doctor` to verify MCP readiness",
                    "category": "index_not_ready"
                }),
            ),
            (
                leantoken::Error::StaleCursor,
                serde_json::json!({
                    "error": "stale cursor",
                    "category": "stale_cursor"
                }),
            ),
            (
                leantoken::Error::Cancelled,
                serde_json::json!({
                    "error": "request cancelled",
                    "category": "request_cancelled"
                }),
            ),
            (
                leantoken::Error::Io(std::io::Error::other("private descriptor")),
                serde_json::json!({
                    "error": "I/O error: private descriptor",
                    "category": "internal_error"
                }),
            ),
            (
                leantoken::Error::InvalidRequest("bad flags".into()),
                serde_json::json!({
                    "error": "invalid request: bad flags",
                    "category": "invalid_request"
                }),
            ),
            (
                leantoken::Error::InternalFailure("parser returned None".into()),
                serde_json::json!({
                    "error": "invalid request: parser returned None",
                    "category": "internal_error"
                }),
            ),
            (
                leantoken::Error::DoctorFailure {
                    stage: "catalog",
                    message: "tools/list returned no result".into(),
                },
                serde_json::json!({
                    "error": "doctor catalog check failed: tools/list returned no result",
                    "category": "doctor_failure",
                    "stage": "catalog"
                }),
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(
                serde_json::to_value(cli_error_response(&error))
                    .expect("CLI error response is serializable"),
                expected
            );
        }
    }

    #[test]
    fn cli_parse_error_json_preserves_plain_clap_message() {
        let error = Cli::try_parse_from([
            "leantoken",
            "--json",
            "files",
            "tree",
            "--max-results",
            "nope",
        ])
        .expect_err("invalid numeric argument");

        assert_eq!(
            serde_json::to_value(cli_parse_error_response(&error))
                .expect("CLI parse error response is serializable"),
            serde_json::json!({
                "error": error.to_string().trim_end(),
                "category": "invalid_input"
            })
        );
    }
