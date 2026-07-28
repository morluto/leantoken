fn print<T: Serialize>(value: &T, compact: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if compact {
        serde_json::to_writer(&mut lock, value)?;
    } else {
        serde_json::to_writer_pretty(&mut lock, value)?;
    }
    lock.write_all(b"\n")?;
    Ok(())
}

fn cli_error_message(error: &leantoken::Error) -> String {
    let error = error.reconciliation_cause();
    match error {
        leantoken::Error::IndexNotReady => "repository index is not ready; run `leantoken index` \
            for direct CLI use or `leantoken doctor` to verify MCP readiness"
            .into(),
        _ => error.to_string(),
    }
}

fn cli_json_requested(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument.as_os_str() == OsStr::new("--json"))
}

#[derive(Debug, Serialize)]
struct CliErrorResponse {
    error: String,
    category: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    violations: Option<Vec<leantoken::InputViolation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    syntax_category: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
}

fn cli_parse_error_response(error: &clap::Error) -> CliErrorResponse {
    use clap::error::ErrorKind;

    let category = match error.kind() {
        ErrorKind::InvalidValue
        | ErrorKind::UnknownArgument
        | ErrorKind::InvalidSubcommand
        | ErrorKind::NoEquals
        | ErrorKind::ValueValidation
        | ErrorKind::TooManyValues
        | ErrorKind::TooFewValues
        | ErrorKind::WrongNumberOfValues
        | ErrorKind::ArgumentConflict
        | ErrorKind::MissingRequiredArgument
        | ErrorKind::MissingSubcommand
        | ErrorKind::InvalidUtf8
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => "invalid_input",
        _ => "internal_error",
    };

    CliErrorResponse {
        error: error.to_string().trim_end().to_owned(),
        category,
        stage: None,
        field: None,
        requested: None,
        limit: None,
        reason: None,
        violations: None,
        syntax_category: None,
        offset: None,
        byte_offset: None,
        line: None,
        column: None,
    }
}

fn cli_error_response(error: &leantoken::Error) -> CliErrorResponse {
    let error = error.reconciliation_cause();
    let (category, stage, field, requested, limit) = match error {
        leantoken::Error::DoctorFailure { stage, .. } => {
            ("doctor_failure", Some(*stage), None, None, None)
        }
        leantoken::Error::InvalidInput { field, .. } => {
            ("invalid_input", None, Some(*field), None, None)
        }
        leantoken::Error::InvalidInputConstraints(_) => ("invalid_input", None, None, None, None),
        leantoken::Error::InvalidJson { .. } => ("invalid_json", None, Some("path"), None, None),
        leantoken::Error::InvalidJsonSelector { stage, .. } => (
            "invalid_json_selector",
            Some(*stage),
            Some("JMESPath expression"),
            None,
            None,
        ),
        leantoken::Error::InputTooLong { field, max_bytes } => {
            ("input_too_long", None, Some(*field), None, Some(*max_bytes))
        }
        leantoken::Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        } => (
            "request_limit_exceeded",
            None,
            Some(*field),
            Some(*requested),
            Some(*limit),
        ),
        leantoken::Error::LimitExceeded => ("request_limit_exceeded", None, None, None, None),
        leantoken::Error::NotIndexed(_) => ("not_indexed", None, None, None, None),
        leantoken::Error::SymbolNotFound { .. } => ("symbol_not_found", None, None, None, None),
        leantoken::Error::HeadingNotFound { .. } => ("heading_not_found", None, None, None, None),
        leantoken::Error::IndexNotReady => ("index_not_ready", None, None, None, None),
        leantoken::Error::StaleCursor => ("stale_cursor", None, None, None, None),
        leantoken::Error::UnknownReceipt(_) => ("unknown_receipt", None, None, None, None),
        leantoken::Error::StaleReceipt { .. } => ("stale_receipt", None, None, None, None),
        leantoken::Error::Cancelled => ("request_cancelled", None, None, None, None),
        leantoken::Error::PathOutsideRoot(_) => ("path_outside_root", None, None, None, None),
        leantoken::Error::UnsupportedPathEncoding(_) => {
            ("unsupported_path_encoding", None, None, None, None)
        }
        leantoken::Error::UnsupportedLanguage(_) => {
            ("unsupported_language", None, None, None, None)
        }
        leantoken::Error::InvalidRequest(_) => ("invalid_request", None, None, None, None),
        leantoken::Error::Regex(_) => ("invalid_regex", None, None, None, None),
        leantoken::Error::Glob(_) => ("invalid_glob", None, None, None, None),
        leantoken::Error::RootNotFound(_)
        | leantoken::Error::UnsafeRepositoryRoot(_)
        | leantoken::Error::RepositoryMismatch { .. }
        | leantoken::Error::InvalidConfiguration(_) => {
            ("repository_configuration", None, None, None, None)
        }
        leantoken::Error::IndexLimitExceeded { .. } => {
            ("repository_index_limit", None, None, None, None)
        }
        leantoken::Error::RepositoryTraversal(_) => {
            ("repository_traversal", None, None, None, None)
        }
        leantoken::Error::RuntimeCapabilityUnavailable { .. } => {
            ("runtime_unavailable", None, None, None, None)
        }
        leantoken::Error::StaleReconciliation { .. } | leantoken::Error::RetryableConflict(_) => {
            ("retryable_conflict", None, None, None, None)
        }
        _ => ("internal_error", None, None, None, None),
    };
    let (reason, syntax_category, offset, byte_offset, line, column) = match error {
        leantoken::Error::InvalidJson {
            syntax_category,
            reason,
            byte_offset,
            line,
            column,
            ..
        } => (
            Some(reason.clone()),
            Some(*syntax_category),
            None,
            Some(*byte_offset),
            Some(*line),
            Some(*column),
        ),
        leantoken::Error::InvalidJsonSelector {
            reason,
            offset,
            line,
            column,
            ..
        } => (
            Some(reason.clone()),
            None,
            Some(*offset),
            None,
            Some(*line),
            Some(*column),
        ),
        _ => (None, None, None, None, None, None),
    };
    let violations = match error {
        leantoken::Error::InvalidInputConstraints(violations) => {
            Some(violations.as_slice().to_vec())
        }
        _ => None,
    };

    CliErrorResponse {
        error: cli_error_message(error),
        category,
        stage,
        field,
        requested,
        limit,
        reason,
        violations,
        syntax_category,
        offset,
        byte_offset,
        line,
        column,
    }
}

fn init_tracing(json: bool) {
    if json {
        return;
    }

    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::FilterFn;
    use tracing_subscriber::prelude::*;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    // Safety-net filter: reject any log event that carries a field name which
    // could contain source content.  By design, LeanToken logs only paths,
    // counts, hashes, timings, and error summaries.  This filter acts as a
    // structural invariant that prevents source bodies from ever appearing in
    // structured log output.
    let scrub_fields = FilterFn::new(|meta: &tracing::Metadata<'_>| -> bool {
        let forbidden = [
            "source_body",
            "source_text",
            "file_content",
            "body",
            "token_text",
        ];
        let contains_source = meta
            .fields()
            .iter()
            .any(|field| forbidden.contains(&field.name()));
        let contains_rmcp_payload = meta.target().starts_with("rmcp")
            && meta
                .fields()
                .iter()
                .any(|field| matches!(field.name(), "request" | "result"));
        !contains_source && !contains_rmcp_payload
    });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_filter(env_filter)
                .with_filter(scrub_fields),
        )
        .init();
}
