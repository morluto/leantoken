pub(super) fn validate_search_input(request: &SearchRequest) -> Result<()> {
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
    validate_optional_input(request.cursor.as_deref(), "cursor", MAX_CURSOR_BYTES)?;
    Ok(())
}

pub(super) fn parse_search_kind(
    request: &SearchRequest,
    output_shape: SearchOutputShape,
) -> Result<SearchKind> {
    let exhaustive_mode = match (request.mode, request.all_occurrences) {
        (_, false) => None,
        (SearchMode::Text, true) => Some(ExhaustiveSearchMode::Text),
        (SearchMode::Regex, true) => Some(ExhaustiveSearchMode::Regex),
        (mode, true) => {
            return Err(incompatible_occurrence_options(
                mode,
                vec!["all_occurrences=true".into()],
            ));
        }
    };
    if request.prefer_structural
        && !matches!(request.mode, SearchMode::Auto | SearchMode::Identifier)
    {
        return Err(Error::InvalidInput {
            field: "prefer structural",
            reason: "requires auto or identifier mode",
        });
    }
    if request.query_receipt.is_some() {
        if !request.all_occurrences || !matches!(request.mode, SearchMode::Text | SearchMode::Regex)
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
    if matches!(output_shape, SearchOutputShape::OccurrenceGroups(_)) && !request.all_occurrences {
        return Err(Error::InvalidInput {
            field: "occurrence projection",
            reason: "requires all_occurrences=true",
        });
    }
    if request.query_receipt.is_some()
        && !matches!(output_shape, SearchOutputShape::OccurrenceGroups(_))
    {
        return Err(Error::InvalidInput {
            field: "query_receipt",
            reason: "requires the occurrences projection",
        });
    }

    let preference = DefinitionPreference::from_prefer_structural(request.prefer_structural);
    let query_receipt = match &request.query_receipt {
        None => PreparedQueryReceipt::None,
        Some(QueryReceiptAction::Record) => {
            PreparedQueryReceipt::Record(ExactQueryPredicate::from_request(request)?)
        }
        Some(QueryReceiptAction::Reuse { receipt_id }) => PreparedQueryReceipt::Reuse {
            receipt_id: receipt_id.clone(),
            predicate: ExactQueryPredicate::from_request(request)?,
        },
    };
    if let Some(mode) = exhaustive_mode {
        return Ok(SearchKind::Exhaustive {
            mode,
            query_receipt,
        });
    }
    Ok(match request.mode {
        SearchMode::Auto => SearchKind::Auto(preference),
        SearchMode::Text => SearchKind::Text,
        SearchMode::Regex => SearchKind::Regex,
        SearchMode::Identifier => SearchKind::Identifier(preference),
        SearchMode::Symbol => SearchKind::Symbol,
        SearchMode::Reference => SearchKind::Reference,
    })
}
use super::*;
