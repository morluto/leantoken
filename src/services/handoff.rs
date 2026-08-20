use std::collections::BTreeSet;

use crate::model::{
    ContextRequest, ContextResponse, HandoffEvidence, HandoffManifest, HandoffManifestRequest,
    HandoffWorkingTreeState,
};
use crate::services::validation::{validate_input, validate_optional_input};
use crate::{Error, Result};

const HANDOFF_SCHEMA_VERSION: u32 = 1;
const MAX_SUMMARY_BYTES: usize = 512;
const MAX_NOTE_BYTES: usize = 512;
const MAX_VALIDATION_COMMAND_BYTES: usize = 1024;
const MAX_HOST_ITEMS: usize = 16;
const MAX_EVIDENCE: usize = 100;
const MAX_HELD_HASHES: usize = 64;
const MAX_FOCUS_ITEMS: usize = 32;
const MAX_CHANGED_PATHS: usize = 64;
const MAX_RELATED_PATHS: usize = 64;
const MAX_TEST_PATHS: usize = 32;
const MAX_GAPS: usize = 64;

pub(super) struct HandoffProvenance {
    pub(super) commit_revision: Option<String>,
    pub(super) commit_revision_available: bool,
    pub(super) working_tree_state: HandoffWorkingTreeState,
    pub(super) working_tree_paths: Vec<String>,
    pub(super) provenance: Option<crate::model::RepositoryProvenance>,
}

pub(super) fn parse_request(mut request: HandoffManifestRequest) -> Result<HandoffManifestRequest> {
    validate_optional_input(
        request.summary.as_deref(),
        "handoff.summary",
        MAX_SUMMARY_BYTES,
    )?;
    if request
        .summary
        .as_deref()
        .is_some_and(|summary| summary.trim().is_empty())
    {
        return Err(invalid_input("handoff.summary", "must not be empty"));
    }
    validate_host_notes(&request.assumptions, "handoff.assumptions")?;
    validate_host_notes(&request.open_questions, "handoff.open_questions")?;
    validate_host_notes(&request.negative_evidence, "handoff.negative_evidence")?;
    validate_host_notes(&request.avoid_rules, "handoff.avoid_rules")?;
    if request.validations.len() > MAX_HOST_ITEMS {
        return Err(request_limit(
            "handoff.validations",
            request.validations.len(),
            MAX_HOST_ITEMS,
        ));
    }
    for validation in &request.validations {
        validate_input(
            &validation.command,
            "handoff.validations.command",
            MAX_VALIDATION_COMMAND_BYTES,
        )?;
        if validation.command.trim().is_empty() {
            return Err(invalid_input(
                "handoff.validations.command",
                "must not be empty",
            ));
        }
        validate_optional_input(
            validation.summary.as_deref(),
            "handoff.validations.summary",
            MAX_NOTE_BYTES,
        )?;
        if validation
            .summary
            .as_deref()
            .is_some_and(|summary| summary.trim().is_empty())
        {
            return Err(invalid_input(
                "handoff.validations.summary",
                "must not be empty",
            ));
        }
    }
    request.summary = request.summary.map(|summary| summary.trim().to_owned());
    for values in [
        &mut request.assumptions,
        &mut request.open_questions,
        &mut request.negative_evidence,
        &mut request.avoid_rules,
    ] {
        for value in values {
            *value = value.trim().to_owned();
        }
    }
    for validation in &mut request.validations {
        validation.command = validation.command.trim().to_owned();
        validation.summary = validation
            .summary
            .take()
            .map(|summary| summary.trim().to_owned());
    }
    Ok(request)
}

pub(super) fn build(
    request: &ContextRequest,
    handoff: &HandoffManifestRequest,
    response: &ContextResponse,
    mut evidence: Vec<HandoffEvidence>,
    provenance: HandoffProvenance,
) -> HandoffManifest {
    let mut gaps = Vec::new();
    evidence.sort_by(|left, right| {
        (
            left.path.as_str(),
            left.start_line,
            left.end_line,
            left.content_hash.as_str(),
        )
            .cmp(&(
                right.path.as_str(),
                right.start_line,
                right.end_line,
                right.content_hash.as_str(),
            ))
    });
    evidence.dedup();
    truncate_items(&mut evidence, MAX_EVIDENCE, "selected evidence", &mut gaps);

    let summary = handoff.summary.clone().unwrap_or_else(|| {
        let (summary, truncated) = truncate_utf8(&request.task, MAX_SUMMARY_BYTES);
        if truncated {
            gaps.push(format!(
                "task summary truncated to {MAX_SUMMARY_BYTES} UTF-8 bytes"
            ));
        }
        summary
    });
    if !provenance.commit_revision_available {
        gaps.push("Git commit identity was unavailable".into());
    }
    if provenance.working_tree_state == HandoffWorkingTreeState::Unknown {
        gaps.push("Git working-tree state was unavailable".into());
    }

    let (mut changed_paths_complete, mut changed_paths_limit) =
        response.diff_scope.as_ref().map_or(
            (
                provenance
                    .provenance
                    .as_ref()
                    .is_some_and(|value| value.working_tree_paths_complete),
                provenance
                    .provenance
                    .as_ref()
                    .and_then(|value| value.working_tree_paths_limit),
            ),
            |scope| (scope.changed_paths_complete, scope.changed_paths_limit),
        );
    if !changed_paths_complete {
        let bound = changed_paths_limit
            .map_or_else(|| "the configured bound".into(), |limit| limit.to_string());
        gaps.push(format!(
            "changed-path discovery was incomplete at {bound} paths"
        ));
    }
    let mut changed_paths = response.diff_scope.as_ref().map_or_else(
        || provenance.working_tree_paths.clone(),
        |scope| scope.changed_paths.clone(),
    );
    let mut related_paths = Vec::new();
    let mut test_paths = Vec::new();
    if let Some(diff_evidence) = response
        .diff_scope
        .as_ref()
        .and_then(|scope| scope.evidence.as_ref())
    {
        for related in &diff_evidence.related_paths {
            related_paths.push(related.related_path.clone());
            if related.signal == "test_name_match" || is_likely_test_path(&related.related_path) {
                test_paths.push(related.related_path.clone());
            }
        }
        if let Some(semantic_change) = &diff_evidence.semantic_change {
            for owner_test in &semantic_change.owner_tests {
                test_paths.extend(owner_test.paths.iter().cloned());
            }
            gaps.extend(
                semantic_change
                    .gaps
                    .iter()
                    .map(|gap| format!("semantic diff: {gap}")),
            );
        }
        gaps.extend(
            diff_evidence
                .gaps
                .iter()
                .map(|gap| format!("diff evidence: {gap}")),
        );
    }

    let held_fragment_hashes = bounded_sorted(
        request.known_hashes.clone(),
        MAX_HELD_HASHES,
        "held fragment hashes",
        &mut gaps,
    );
    let focus_paths = bounded_sorted(
        request.focus_paths.clone(),
        MAX_FOCUS_ITEMS,
        "focus paths",
        &mut gaps,
    );
    let focus_symbols = bounded_sorted(
        request.focus_symbols.clone(),
        MAX_FOCUS_ITEMS,
        "focus symbols",
        &mut gaps,
    );
    if changed_paths.len() > MAX_CHANGED_PATHS {
        changed_paths_complete = false;
        changed_paths_limit.get_or_insert(MAX_CHANGED_PATHS);
    }
    changed_paths = bounded_sorted(changed_paths, MAX_CHANGED_PATHS, "changed paths", &mut gaps);
    related_paths = bounded_sorted(related_paths, MAX_RELATED_PATHS, "related paths", &mut gaps);
    test_paths = bounded_sorted(test_paths, MAX_TEST_PATHS, "test paths", &mut gaps);
    gaps.sort();
    gaps.dedup();
    truncate_gaps(&mut gaps);

    HandoffManifest {
        schema_version: HANDOFF_SCHEMA_VERSION,
        summary,
        task_fingerprint: response.receipt.task_fingerprint.clone(),
        repository_id: response.meta.repository_id.clone(),
        repository_generation: response.meta.repository_generation,
        freshness: response.meta.freshness.clone(),
        provenance: provenance.provenance,
        commit_revision: provenance.commit_revision,
        working_tree_state: provenance.working_tree_state,
        base_revision: response
            .diff_scope
            .as_ref()
            .and_then(|scope| scope.base_revision.clone()),
        head_revision: response
            .diff_scope
            .as_ref()
            .and_then(|scope| scope.head_revision.clone()),
        receipt_id: response.meta.receipt_id.clone(),
        evidence,
        held_fragment_hashes,
        focus_paths,
        focus_symbols,
        changed_paths,
        changed_paths_complete,
        changed_paths_limit,
        related_paths,
        test_paths,
        validations: handoff.validations.clone(),
        assumptions: handoff.assumptions.clone(),
        open_questions: handoff.open_questions.clone(),
        negative_evidence: handoff.negative_evidence.clone(),
        avoid_rules: handoff.avoid_rules.clone(),
        gaps,
    }
}

fn validate_host_notes(values: &[String], field: &'static str) -> Result<()> {
    if values.len() > MAX_HOST_ITEMS {
        return Err(request_limit(field, values.len(), MAX_HOST_ITEMS));
    }
    for value in values {
        validate_input(value, field, MAX_NOTE_BYTES)?;
        if value.trim().is_empty() {
            return Err(invalid_input(field, "items must not be empty"));
        }
    }
    Ok(())
}

fn invalid_input(field: &'static str, reason: &'static str) -> Error {
    Error::InvalidInput { field, reason }
}

fn request_limit(field: &'static str, requested: usize, limit: usize) -> Error {
    Error::RequestLimitExceeded {
        field,
        requested,
        limit,
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn bounded_sorted(
    values: Vec<String>,
    limit: usize,
    label: &str,
    gaps: &mut Vec<String>,
) -> Vec<String> {
    let mut values = values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    truncate_items(&mut values, limit, label, gaps);
    values
}

fn truncate_items<T>(values: &mut Vec<T>, limit: usize, label: &str, gaps: &mut Vec<String>) {
    if values.len() <= limit {
        return;
    }
    let omitted = values.len() - limit;
    values.truncate(limit);
    gaps.push(format!(
        "{label} truncated; {omitted} additional items omitted"
    ));
}

fn truncate_gaps(gaps: &mut Vec<String>) {
    if gaps.len() <= MAX_GAPS {
        return;
    }
    let retained = MAX_GAPS - 1;
    let omitted = gaps.len() - retained;
    gaps.truncate(retained);
    gaps.push(format!(
        "manifest gaps truncated; {omitted} additional items omitted"
    ));
}

fn is_likely_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("tests/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("__tests__")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with("test_") || name.ends_with("_test.rs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_utf8_keeps_valid_boundaries() {
        let value = "a".repeat(MAX_SUMMARY_BYTES - 1) + "界";
        let (truncated, was_truncated) = truncate_utf8(&value, MAX_SUMMARY_BYTES);
        assert!(was_truncated);
        assert_eq!(truncated.len(), MAX_SUMMARY_BYTES - 1);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn likely_test_path_requires_test_shaped_components() {
        assert!(is_likely_test_path("tests/services.rs"));
        assert!(is_likely_test_path("src/parser.test.ts"));
        assert!(!is_likely_test_path("src/latest.rs"));
    }

    #[test]
    fn host_item_limits_return_the_named_request_boundary() {
        let request = HandoffManifestRequest {
            assumptions: vec!["bounded".into(); MAX_HOST_ITEMS + 1],
            ..HandoffManifestRequest::default()
        };
        assert!(matches!(
            parse_request(request),
            Err(Error::RequestLimitExceeded {
                field: "handoff.assumptions",
                requested,
                limit: MAX_HOST_ITEMS,
            }) if requested == MAX_HOST_ITEMS + 1
        ));
    }

    #[test]
    fn gap_limit_retains_an_explicit_truncation_diagnostic() {
        let mut gaps = (0..MAX_GAPS + 4)
            .map(|index| format!("gap {index}"))
            .collect::<Vec<_>>();
        truncate_gaps(&mut gaps);
        assert_eq!(gaps.len(), MAX_GAPS);
        assert_eq!(
            gaps.last().map(String::as_str),
            Some("manifest gaps truncated; 5 additional items omitted")
        );
    }
}
