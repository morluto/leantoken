use std::collections::BTreeSet;

use super::super::{
    ServiceCallOptions, Services,
    receipts::{ReceiptDecision, ReceiptEvidence},
};
use super::ContextPolicy;
use super::RepositoryGeneration;
use crate::{
    Error, Result,
    model::{ContextCoverageReceipt, ContextRequest, ContextResponse, ContextResponseProfile},
};

pub(super) fn effective_context_response_profile(
    request: &ContextRequest,
    options: ServiceCallOptions,
) -> Result<ContextResponseProfile> {
    match (
        options.context_response_profile(),
        request.explain_diagnostics,
    ) {
        (Some(ContextResponseProfile::Compact | ContextResponseProfile::Balanced), true) => {
            Err(Error::InvalidInput {
                field: "response_profile",
                reason: "explain_diagnostics=true requires response_profile=explain",
            })
        }
        (Some(profile), _) => Ok(profile),
        (None, true) => Ok(ContextResponseProfile::Explain),
        (None, false) => Ok(ContextResponseProfile::Balanced),
    }
}

pub(super) struct ContextResponseFinalization<'a> {
    pub(super) session: &'a RepositoryGeneration,
    pub(super) request: &'a ContextRequest,
    pub(super) policy: &'a ContextPolicy,
    pub(super) options: ServiceCallOptions,
    pub(super) generation: u64,
}

pub(super) fn merge_selected_coverage(
    mut coverage: ContextCoverageReceipt,
    response: &mut ContextResponse,
) -> ContextCoverageReceipt {
    coverage.covered_must_include_paths =
        std::mem::take(&mut response.coverage.covered_must_include_paths);
    coverage.covered_must_include_symbols =
        std::mem::take(&mut response.coverage.covered_must_include_symbols);
    coverage.partial_must_include_symbols =
        std::mem::take(&mut response.coverage.partial_must_include_symbols);
    coverage.uncovered_must_include_paths =
        std::mem::take(&mut response.coverage.uncovered_must_include_paths);
    coverage.uncovered_must_include_symbols =
        std::mem::take(&mut response.coverage.uncovered_must_include_symbols);
    for (target, selected) in coverage
        .required_evidence
        .iter_mut()
        .zip(std::mem::take(&mut response.coverage.required_evidence))
    {
        target.matched_queries = selected.matched_queries;
        target.unmatched_queries = selected.unmatched_queries;
        target.selected_fragments = selected.selected_fragments;
        target.satisfied = target.indexed_paths > 0 && selected.satisfied;
    }
    for (target, selected) in coverage
        .focus_path_coverage
        .iter_mut()
        .zip(std::mem::take(&mut response.coverage.focus_path_coverage))
    {
        if let Some(mut selected) = selected.diagnostics {
            selected.eligible_paths = target
                .diagnostics
                .as_ref()
                .map_or(0, |diagnostics| diagnostics.eligible_paths);
            target.diagnostics = Some(selected);
        }
    }
    if !coverage.required_evidence.is_empty() {
        coverage.evidence_scope_satisfied =
            Some(coverage.required_evidence.iter().all(|item| item.satisfied));
    }
    coverage
        .uncovered_must_include_paths
        .retain(|pattern| !coverage.unmatched_must_include_paths.contains(pattern));
    coverage
        .uncovered_must_include_symbols
        .retain(|symbol| !coverage.unmatched_must_include_symbols.contains(symbol));
    coverage
}

pub(super) fn selected_paths(response: &ContextResponse) -> Vec<String> {
    response.plan.as_ref().map_or_else(
        || {
            response
                .fragments
                .iter()
                .map(|fragment| fragment.path.clone())
                .collect()
        },
        |plan| {
            plan.candidates
                .iter()
                .map(|candidate| candidate.path.clone())
                .collect()
        },
    )
}

pub(super) fn append_coverage_warnings(response: &mut ContextResponse) {
    let uncovered = response
        .coverage
        .uncovered_must_include_paths
        .len()
        .saturating_add(response.coverage.uncovered_must_include_symbols.len());
    if uncovered > 0 {
        response.warnings.push(format!(
            "{uncovered} indexed must-cover requirements were not selected"
        ));
    }
    let partial = response.coverage.partial_must_include_symbols.len();
    if partial > 0 {
        let subject = if partial == 1 {
            "1 required symbol was".to_owned()
        } else {
            format!("{partial} required symbols were")
        };
        response.warnings.push(format!(
            "{subject} returned only partially; inspect target ranges and truncated fragments"
        ));
    }
    let unmatched = response
        .coverage
        .unmatched_must_include_paths
        .len()
        .saturating_add(response.coverage.unmatched_must_include_symbols.len());
    if unmatched > 0 {
        response.warnings.push(format!(
            "{unmatched} must-cover requirements matched no indexed evidence"
        ));
    }
    let unsatisfied_evidence = response
        .coverage
        .required_evidence
        .iter()
        .filter(|item| !item.satisfied)
        .count();
    if unsatisfied_evidence > 0 {
        response.warnings.push(format!(
            "{unsatisfied_evidence} required evidence contracts lacked matching selected evidence"
        ));
    }
    let bounded_evidence_paths = response
        .coverage
        .required_evidence
        .iter()
        .filter(|item| item.indexed_paths > item.inspected_paths)
        .count();
    if bounded_evidence_paths > 0 {
        response.warnings.push(format!(
            "{bounded_evidence_paths} required evidence contracts matched more indexed paths than the bounded local inspection covered"
        ));
    }
    let unmatched_hints = response
        .coverage
        .unmatched_focus_paths
        .len()
        .saturating_add(response.coverage.unmatched_focus_symbols.len())
        .saturating_add(response.coverage.unmatched_include_paths.len());
    if unmatched_hints > 0 {
        response.warnings.push(format!(
            "{unmatched_hints} focus or include constraints matched no indexed evidence"
        ));
    }
    let underfilled_focus_paths = response
        .coverage
        .focus_path_coverage
        .iter()
        .filter(|focus| !focus.satisfied)
        .count();
    if underfilled_focus_paths > 0 {
        response.warnings.push(format!(
            "{underfilled_focus_paths} focus path constraints did not meet minimum fragment coverage"
        ));
    }
    if response
        .coverage
        .changed_path_coverage
        .as_ref()
        .is_some_and(|changed| !changed.satisfied)
    {
        response
            .warnings
            .push("strict changed-path scope produced no indexed selected evidence".into());
    }
}

impl Services {
    pub(super) fn finalize_context_delivery(
        &self,
        response: &mut ContextResponse,
        finalization: ContextResponseFinalization<'_>,
    ) -> Result<Option<usize>> {
        let ContextResponseFinalization {
            session,
            request,
            policy,
            options,
            generation,
        } = finalization;
        if options.max_response_tokens().is_some() {
            self.fit_context_response(response, request, policy, options)?;
        }
        if !policy.is_plan() {
            let receipt_candidates = response
                .fragments
                .iter()
                .map(|fragment| {
                    ReceiptEvidence::new(
                        fragment.path.clone(),
                        fragment.start_line,
                        fragment.end_line,
                        fragment.content_hash.clone(),
                        Some(&fragment.content),
                    )
                })
                .collect::<Vec<_>>();
            let receipt =
                self.evaluate_receipt(policy.receipt_id(), generation, &receipt_candidates)?;
            response.fragments = response
                .fragments
                .drain(..)
                .zip(&receipt.decisions)
                .filter_map(|(fragment, decision)| {
                    matches!(
                        decision,
                        ReceiptDecision::Return | ReceiptDecision::ReturnNearDuplicate
                    )
                    .then_some(fragment)
                })
                .collect();
            response.receipt.fragment_hashes = response
                .fragments
                .iter()
                .map(|fragment| fragment.content_hash.clone())
                .collect();
            response.meta.source_tokens = response
                .fragments
                .iter()
                .map(|fragment| self.config.tokenizer.count(&fragment.content))
                .sum();
            receipt.apply_meta(&mut response.meta);
            if response.meta.receipt_near_duplicates > 0 {
                response.warnings.push(format!(
                    "{} returned fragments are semantic near-duplicates of prior receipt evidence",
                    response.meta.receipt_near_duplicates
                ));
            }
            if response.fragments.is_empty() {
                if response.meta.receipt_suppressed_exact + response.meta.receipt_suppressed_overlap
                    > 0
                {
                    response
                        .warnings
                        .push("all selected evidence was already covered by the receipt".into());
                } else if response.omission_summary.budget_or_result_limit == 0 {
                    response
                        .warnings
                        .push("no relevant indexed evidence found".into());
                }
            }
        }
        if let Some(manifest) = &mut response.handoff_manifest {
            manifest.receipt_id.clone_from(&response.meta.receipt_id);
        }
        if options.mcp_response_shape().is_some() {
            self.finalize_bounded_response(response, options)?;
        } else {
            self.finalize_response(response)?;
        }
        if let Some(max_response_tokens) = options.max_response_tokens()
            && response.meta.total_response_tokens > max_response_tokens
        {
            return Err(Error::ResponseAccountingInvariant(
                "context response exceeded its fitted serialized-response budget".into(),
            ));
        }
        if policy.is_plan() {
            return Ok(None);
        }
        let paths = response
            .fragments
            .iter()
            .map(|fragment| fragment.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        session.whole_file_source_tokens(&paths, self.config.tokenizer.name())
    }
}
