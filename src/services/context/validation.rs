impl Services {
    pub(super) fn validate_workflow_evidence(&self, evidence: &WorkflowEvidence) -> Result<()> {
        let classes = [
            ("failure_traces", &evidence.failure_traces),
            ("symbols", &evidence.symbols),
            ("paths", &evidence.paths),
            ("test_intents", &evidence.test_intents),
        ];
        let mut total_bytes = 0usize;
        for (field, values) in classes {
            if values.len() > MAX_WORKFLOW_EVIDENCE_ITEMS_PER_CLASS {
                return Err(Error::RequestLimitExceeded {
                    field: "workflow_evidence items per class",
                    requested: values.len(),
                    limit: MAX_WORKFLOW_EVIDENCE_ITEMS_PER_CLASS,
                });
            }
            for value in values {
                validate_input(value, field, MAX_WORKFLOW_EVIDENCE_ITEM_BYTES)?;
                if value.trim().is_empty() {
                    return Err(Error::InvalidInput {
                        field: "workflow_evidence",
                        reason: "must not contain empty values",
                    });
                }
                total_bytes = total_bytes.saturating_add(value.len());
            }
        }
        if total_bytes > MAX_WORKFLOW_EVIDENCE_TOTAL_BYTES {
            return Err(Error::RequestLimitExceeded {
                field: "workflow_evidence total bytes",
                requested: total_bytes,
                limit: MAX_WORKFLOW_EVIDENCE_TOTAL_BYTES,
            });
        }
        for path in &evidence.paths {
            validate_relative(path)?;
        }
        Ok(())
    }

    pub(super) fn validate_context_request(
        &self,
        request: &ContextRequest,
        handoff: Option<&HandoffManifestRequest>,
    ) -> Result<()> {
        validate_context_option_constraints(request, handoff)?;
        if request.task.trim().is_empty() {
            return Err(Error::InvalidInput {
                field: "task",
                reason: "must not be empty",
            });
        }
        self.token_budget_limit(request.token_budget)?;
        if let Some(max_fragments) = request.max_fragments {
            self.result_limit(Some(max_fragments))?;
        }
        if let Some(minimum) = request.minimum_fragments_per_focus_path {
            self.result_limit(Some(minimum))?;
            if minimum > MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN {
                return Err(Error::RequestLimitExceeded {
                    field: "minimum_fragments_per_focus_path",
                    requested: minimum,
                    limit: MAX_CONTEXT_FOCUS_CANDIDATES_PER_PATTERN,
                });
            }
        }
        if request.focus_paths.len() > MAX_CONTEXT_FOCUS_PATTERNS {
            return Err(Error::RequestLimitExceeded {
                field: "focus_paths",
                requested: request.focus_paths.len(),
                limit: MAX_CONTEXT_FOCUS_PATTERNS,
            });
        }
        validate_input(&request.task, "task", MAX_QUERY_BYTES)?;
        if request
            .include_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "include paths",
                reason: "must not contain empty patterns",
            });
        }
        validate_glob_patterns(&request.include_paths)?;
        if request
            .must_include_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "must include paths",
                reason: "must not contain empty patterns",
            });
        }
        validate_glob_patterns(&request.must_include_paths)?;
        if request
            .focus_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "focus paths",
                reason: "must not contain empty patterns",
            });
        }
        validate_glob_patterns(&request.focus_paths)?;
        if request
            .exclude_paths
            .iter()
            .any(|pattern| pattern.trim().is_empty())
        {
            return Err(Error::InvalidInput {
                field: "exclude paths",
                reason: "must not contain empty patterns",
            });
        }
        validate_glob_patterns(&request.exclude_paths)?;
        if request.focus_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.focus_symbols {
            validate_input(symbol, "focus symbol", MAX_PATTERN_BYTES)?;
            if symbol.trim().is_empty() {
                return Err(Error::InvalidInput {
                    field: "focus symbols",
                    reason: "must not contain empty symbols",
                });
            }
        }
        if request.must_include_symbols.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for symbol in &request.must_include_symbols {
            validate_input(symbol, "must include symbol", MAX_PATTERN_BYTES)?;
            if symbol.trim().is_empty() {
                return Err(Error::InvalidInput {
                    field: "must include symbols",
                    reason: "must not contain empty symbols",
                });
            }
        }
        if request.required_evidence.len() > MAX_CONTEXT_REQUIRED_EVIDENCE {
            return Err(Error::RequestLimitExceeded {
                field: "required_evidence",
                requested: request.required_evidence.len(),
                limit: MAX_CONTEXT_REQUIRED_EVIDENCE,
            });
        }
        let mut evidence_query_bytes = 0usize;
        for requirement in &request.required_evidence {
            validate_glob_patterns(std::slice::from_ref(&requirement.path))?;
            if requirement.path.trim().is_empty() {
                return Err(Error::InvalidInput {
                    field: "required_evidence path",
                    reason: "must not be empty",
                });
            }
            if requirement.queries.is_empty() {
                return Err(Error::InvalidInput {
                    field: "required_evidence queries",
                    reason: "must not be empty",
                });
            }
            if requirement.queries.len() > MAX_CONTEXT_EVIDENCE_QUERIES {
                return Err(Error::RequestLimitExceeded {
                    field: "required_evidence queries",
                    requested: requirement.queries.len(),
                    limit: MAX_CONTEXT_EVIDENCE_QUERIES,
                });
            }
            if requirement.minimum_query_matches == 0
                || requirement.minimum_query_matches > requirement.queries.len()
            {
                return Err(Error::InvalidInput {
                    field: "required_evidence minimum_query_matches",
                    reason: "must be between one and the number of queries",
                });
            }
            for query in &requirement.queries {
                validate_input(query, "required_evidence query", MAX_PATTERN_BYTES)?;
                if query.trim().is_empty() {
                    return Err(Error::InvalidInput {
                        field: "required_evidence queries",
                        reason: "must not contain empty queries",
                    });
                }
                evidence_query_bytes = evidence_query_bytes.saturating_add(query.len());
            }
        }
        if evidence_query_bytes > MAX_CONTEXT_EVIDENCE_QUERY_BYTES {
            return Err(Error::RequestLimitExceeded {
                field: "required_evidence query bytes",
                requested: evidence_query_bytes,
                limit: MAX_CONTEXT_EVIDENCE_QUERY_BYTES,
            });
        }
        if request.known_hashes.len() > MAX_INPUT_ITEMS {
            return Err(Error::LimitExceeded);
        }
        for hash in &request.known_hashes {
            validate_input(hash, "known hash", 128)?;
        }
        if request.changed_paths.len() > MAX_DIFF_CHANGED_PATHS {
            return Err(Error::LimitExceeded);
        }
        for path in &request.changed_paths {
            validate_input(path, "changed path", MAX_PATH_BYTES)?;
            validate_relative(path)?;
        }
        if let Some(revision) = request
            .base_revision
            .as_deref()
            .filter(|revision| !revision.trim().is_empty())
        {
            validate_input(revision, "base revision", MAX_BASE_REVISION_BYTES)?;
            parse_revision_range(revision)?;
        }
        for query in facets::plan(&request.task, MAX_CONTEXT_QUERIES)
            .queries
            .iter()
            .filter(|query| !query.has_facet(FacetKind::TestIntent))
        {
            compile_literal_regex(&query.value, false)?;
        }
        if let Some(handoff) = handoff {
            handoff::validate_request(handoff)?;
        }
        Ok(())
    }
}

pub(super) fn validate_context_option_constraints(
    request: &ContextRequest,
    handoff: Option<&HandoffManifestRequest>,
) -> Result<()> {
    let mut violations = Vec::with_capacity(3);
    if (request.strict_focus_paths || request.minimum_fragments_per_focus_path.is_some())
        && request.focus_paths.is_empty()
    {
        violations.push(crate::InputViolation::new(
            "focus paths",
            "must not be empty when focus path constraints are enabled",
        ));
    }
    if request.plan_only && request.receipt_id.is_some() {
        violations.push(crate::InputViolation::new(
            "receipt_id",
            "must be omitted when plan_only is true",
        ));
    }
    if request.plan_only && handoff.is_some() {
        violations.push(crate::InputViolation::new(
            "plan_only",
            "cannot be combined with a handoff manifest",
        ));
    }
    match violations.len() {
        0 => Ok(()),
        1 => {
            let violation = violations[0];
            Err(Error::InvalidInput {
                field: violation.field,
                reason: violation.reason,
            })
        }
        _ => Err(Error::InvalidInputConstraints(crate::InputViolations::new(
            violations,
        ))),
    }
}

pub(super) fn context_accounting_operation(request: &ContextRequest) -> TokenAccountingOperation {
    if request.plan_only {
        TokenAccountingOperation::ContextPlan
    } else {
        TokenAccountingOperation::Context
    }
}

pub(super) fn set_routing_consistency(
    response: &mut ContextResponse,
    consistency: IndexConsistency,
) {
    if let Some(routing) = &mut response.routing {
        routing.consistency = consistency;
    }
}
use super::*;
