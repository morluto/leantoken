fn validate_search_input(request: &SearchRequest) -> Result<()> {
    if request.query.trim().is_empty() {
        return Err(Error::InvalidInput {
            field: "search query",
            reason: "must not be empty",
        });
    }
    validate_input(&request.query, "search query", MAX_QUERY_BYTES)?;
    validate_glob_patterns(&request.include_paths)?;
    validate_glob_patterns(&request.exclude_paths)?;
    validate_glob_patterns(&request.focus_paths)?;
    validate_cursor(request.cursor.as_deref())?;
    if request.all_occurrences && !matches!(request.mode, SearchMode::Text | SearchMode::Regex) {
        return Err(Error::InvalidInput {
            field: "all occurrences",
            reason: "requires text or regex mode",
        });
    }
    if request.prefer_structural
        && !matches!(request.mode, SearchMode::Auto | SearchMode::Identifier)
    {
        return Err(Error::InvalidInput {
            field: "prefer structural",
            reason: "requires auto or identifier mode",
        });
    }
    if request.query_receipt.is_some() {
        if !request.all_occurrences
            || !matches!(request.mode, SearchMode::Text | SearchMode::Regex)
        {
            return Err(Error::InvalidInput {
                field: "query_receipt",
                reason: "requires all_occurrences=true with text or regex mode",
            });
        }
        if !request.focus_paths.is_empty() {
            return Err(Error::InvalidInput {
                field: "query_receipt",
                reason: "does not allow focus_paths",
            });
        }
        if request.receipt_id.is_some() {
            return Err(Error::InvalidInput {
                field: "query_receipt",
                reason: "cannot be combined with evidence receipt_id",
            });
        }
        if request.cursor.is_some() {
            return Err(Error::InvalidInput {
                field: "query_receipt",
                reason: "does not allow a cursor",
            });
        }
        if let Some(QueryReceiptAction::Reuse { receipt_id }) = &request.query_receipt {
            validate_input(
                receipt_id,
                "query receipt_id",
                crate::query_receipt::MAX_QUERY_RECEIPT_ID_BYTES,
            )?;
        }
    }
    if matches!(request.mode, SearchMode::Regex) {
        compile_regex(request)?;
    } else {
        compile_literal_regex(&request.query, request.case_sensitive)?;
    }
    Ok(())
}

fn validate_occurrence_group_input(request: &SearchRequest) -> Result<()> {
    validate_search_input(request)?;
    if !request.all_occurrences {
        return Err(Error::InvalidInput {
            field: "occurrence projection",
            reason: "requires all_occurrences=true",
        });
    }
    Ok(())
}
