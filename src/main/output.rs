use super::*;

pub(super) fn print<T: Serialize>(value: &T, compact: bool) -> Result<()> {
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

pub(super) fn cli_error_message(error: &leantoken::Error) -> String {
    let error = error.reconciliation_cause();
    match error {
        leantoken::Error::IndexNotReady => "repository index is not ready; run `leantoken index` \
            for direct CLI use or `leantoken doctor` to verify MCP readiness"
            .into(),
        leantoken::Error::RetrievalLimitExceeded { kind, .. } => {
            format!("{error}; {}", kind.guidance())
        }
        leantoken::Error::RegexWorkBudgetExceeded { dimension, .. } => {
            format!("{error}; {}", dimension.guidance())
        }
        _ => error.to_string(),
    }
}

pub(super) fn cli_json_requested(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| argument.as_os_str() == OsStr::new("--json"))
}

#[derive(Debug, Serialize)]
pub(super) struct CliErrorResponse {
    error: String,
    category: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_modes: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflicting_options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ranked_symbol_example: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exhaustive_text_example: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_chunks: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provided_max_response_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum_required_response_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_with_at_least: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    breakdown: Option<leantoken::ResponseBudgetBreakdown>,
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

pub(super) fn cli_parse_error_response(error: &clap::Error) -> CliErrorResponse {
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
        allowed_modes: None,
        conflicting_options: None,
        ranked_symbol_example: None,
        exhaustive_text_example: None,
        requested: None,
        limit: None,
        complete: None,
        candidate_files: None,
        candidate_chunks: None,
        candidate_bytes: None,
        provided_max_response_tokens: None,
        minimum_required_response_tokens: None,
        retry_with_at_least: None,
        breakdown: None,
        reason: None,
        violations: None,
        syntax_category: None,
        offset: None,
        byte_offset: None,
        line: None,
        column: None,
    }
}

pub(super) fn cli_error_response(error: &leantoken::Error) -> CliErrorResponse {
    let error = error.reconciliation_cause();
    let category = error.public_category();
    let (stage, field, requested, limit) = match error {
        leantoken::Error::DoctorFailure { stage, .. } => (Some(*stage), None, None, None),
        leantoken::Error::InvalidInput { field, .. } => (None, Some(*field), None, None),
        leantoken::Error::InvalidSearchOptions { field, .. } => (None, Some(*field), None, None),
        leantoken::Error::InvalidJson { .. } => (None, Some("path"), None, None),
        leantoken::Error::InvalidJsonSelector { stage, .. } => {
            (Some(*stage), Some("JMESPath expression"), None, None)
        }
        leantoken::Error::InputTooLong { field, max_bytes } => {
            (None, Some(*field), None, Some(*max_bytes))
        }
        leantoken::Error::RequestLimitExceeded {
            field,
            requested,
            limit,
        } => (None, Some(*field), Some(*requested), Some(*limit)),
        leantoken::Error::RetrievalLimitExceeded {
            observed, limit, ..
        } => (None, None, Some(*observed), Some(*limit)),
        leantoken::Error::RegexWorkBudgetExceeded { limit, .. } => (None, None, None, Some(*limit)),
        leantoken::Error::ResponseBudgetExceeded {
            provided_max_response_tokens,
            minimum_required_response_tokens,
            ..
        } => (
            None,
            Some("max_response_tokens"),
            Some(*minimum_required_response_tokens),
            Some(*provided_max_response_tokens),
        ),
        _ => (None, None, None, None),
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
        leantoken::Error::RetrievalLimitExceeded { kind, .. } => {
            (Some(kind.as_str().to_owned()), None, None, None, None, None)
        }
        leantoken::Error::RegexWorkBudgetExceeded { dimension, .. } => (
            Some(dimension.as_str().to_owned()),
            None,
            None,
            None,
            None,
            None,
        ),
        _ => (None, None, None, None, None, None),
    };
    let violations = match error {
        leantoken::Error::InvalidInputConstraints(violations) => {
            Some(violations.as_slice().to_vec())
        }
        _ => None,
    };
    let (allowed_modes, conflicting_options, ranked_symbol_example, exhaustive_text_example) =
        match error {
            leantoken::Error::InvalidSearchOptions {
                allowed_modes,
                conflicting_options,
                ranked_symbol_example,
                exhaustive_text_example,
                ..
            } => (
                Some(*allowed_modes),
                Some(conflicting_options.clone()),
                Some(*ranked_symbol_example),
                Some(*exhaustive_text_example),
            ),
            _ => (None, None, None, None),
        };
    let (
        provided_max_response_tokens,
        minimum_required_response_tokens,
        retry_with_at_least,
        breakdown,
    ) = match error {
        leantoken::Error::ResponseBudgetExceeded {
            provided_max_response_tokens,
            minimum_required_response_tokens,
            retry_with_at_least,
            breakdown,
        } => (
            Some(*provided_max_response_tokens),
            Some(*minimum_required_response_tokens),
            Some(*retry_with_at_least),
            Some(*breakdown),
        ),
        _ => (None, None, None, None),
    };
    let (complete, candidate_files, candidate_chunks, candidate_bytes) = match error {
        leantoken::Error::RegexWorkBudgetExceeded {
            candidate_files,
            candidate_chunks,
            candidate_bytes,
            ..
        } => (
            Some(false),
            Some(*candidate_files),
            Some(*candidate_chunks),
            Some(*candidate_bytes),
        ),
        _ => (None, None, None, None),
    };

    CliErrorResponse {
        error: cli_error_message(error),
        category,
        stage,
        field,
        allowed_modes,
        conflicting_options,
        ranked_symbol_example,
        exhaustive_text_example,
        requested,
        limit,
        complete,
        candidate_files,
        candidate_chunks,
        candidate_bytes,
        provided_max_response_tokens,
        minimum_required_response_tokens,
        retry_with_at_least,
        breakdown,
        reason,
        violations,
        syntax_category,
        offset,
        byte_offset,
        line,
        column,
    }
}

pub(super) fn init_tracing(json: bool) {
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
