use super::*;
pub(in crate::ranking) struct BuildContextPlanParams<'a> {
    pub(in crate::ranking) request: &'a ContextRequest,
    pub(in crate::ranking) focus_path_matchers: &'a [PathMatcher],
    pub(in crate::ranking) selected: &'a [ScoredCandidate],
    pub(in crate::ranking) candidate_paths_total: usize,
    pub(in crate::ranking) estimated_source_tokens: usize,
    pub(in crate::ranking) generated_artifact_warning: bool,
    pub(in crate::ranking) result_complete: bool,
    pub(in crate::ranking) policy: ContextSelectionPolicy,
}

pub(in crate::ranking) fn build_context_plan(
    params: BuildContextPlanParams<'_>,
) -> Option<ContextQueryPlan> {
    let BuildContextPlanParams {
        request,
        focus_path_matchers,
        selected,
        candidate_paths_total,
        estimated_source_tokens,
        generated_artifact_warning,
        result_complete,
        policy,
    } = params;
    policy.is_plan().then(|| {
        let minimum_fragments = policy.focus_minimum().unwrap_or(0);
        let focus_coverage = request
            .focus_paths
            .iter()
            .zip(focus_path_matchers)
            .map(|(pattern, matcher)| {
                let candidate_fragments = selected
                    .iter()
                    .filter(|candidate| matcher.is_match(&candidate.candidate.path))
                    .count();
                ContextPlanFocusCoverage {
                    pattern: pattern.clone(),
                    candidate_fragments,
                    minimum_fragments,
                    satisfied: candidate_fragments >= minimum_fragments,
                }
            })
            .collect();
        let candidates = selected
            .iter()
            .map(|scored| {
                let (target_start_line, target_end_line) = scored.candidate.target_lines().unzip();
                ContextPlanCandidate {
                    path: scored.candidate.path.clone(),
                    start_line: scored.candidate.start_line,
                    end_line: scored.candidate.end_line,
                    target_start_line,
                    target_end_line,
                    truncated: scored.candidate.target_truncated(),
                    representation: scored.candidate.representation.clone(),
                    score: (scored.score * 10_000.0).round() / 10_000.0,
                    reasons: scored
                        .candidate
                        .reason()
                        .split("; ")
                        .map(str::to_owned)
                        .collect(),
                    estimated_tokens: scored.token_count,
                }
            })
            .collect();
        ContextQueryPlan {
            candidates,
            candidate_paths_total,
            estimated_source_tokens,
            focus_coverage,
            generated_artifact_warning,
            result_complete,
        }
    })
}

pub(in crate::ranking) fn materialize_context_fragments(
    selected: &[ScoredCandidate],
    estimated_source_tokens: usize,
    policy: ContextSelectionPolicy,
) -> (Vec<ContextFragment>, Vec<String>, usize) {
    if policy.is_plan() {
        return (Vec::new(), Vec::new(), 0);
    }
    let fragments = selected
        .iter()
        .map(|scored| {
            let (target_start_line, target_end_line) = scored.candidate.target_lines().unzip();
            ContextFragment {
                path: scored.candidate.path.clone(),
                start_line: scored.candidate.start_line,
                end_line: scored.candidate.end_line,
                target_start_line,
                target_end_line,
                truncated: scored.candidate.target_truncated(),
                representation: scored.candidate.representation.clone(),
                content: scored.candidate.content.clone(),
                content_hash: scored.content_hash.clone(),
                score: (scored.score * 10_000.0).round() / 10_000.0,
                reason: scored.candidate.reason(),
                token_count: scored.token_count,
            }
        })
        .collect();
    let fragment_hashes = selected
        .iter()
        .map(|scored| scored.content_hash.clone())
        .collect();
    (fragments, fragment_hashes, estimated_source_tokens)
}

pub(in crate::ranking) struct FinalizeContextParams<'a> {
    pub request: &'a ContextRequest,
    pub repository_generation: u64,
    pub tokenizer: tokens::Tokenizer,
    pub plan: Option<ContextQueryPlan>,
    pub fragments: Vec<ContextFragment>,
    pub fragment_hashes: Vec<String>,
    pub emitted_tokens: usize,
    pub omitted: Vec<OmittedCandidate>,
    pub omission_summary: ContextOmissionSummary,
    pub coverage: ContextCoverageReceipt,
    pub warnings: Vec<String>,
}

pub(in crate::ranking) fn finalize_context_response(
    params: FinalizeContextParams<'_>,
) -> ContextResponse {
    let FinalizeContextParams {
        request,
        repository_generation,
        tokenizer,
        plan,
        fragments,
        fragment_hashes,
        emitted_tokens,
        omitted,
        omission_summary,
        coverage,
        warnings,
    } = params;
    let task_hash = blake3::hash(request.task.as_bytes()).to_hex().to_string();
    let receipt = EvidenceReceipt {
        task_fingerprint: task_hash[..32].to_string(),
        fragment_hashes,
    };
    let meta = ResponseMeta {
        repository_id: String::new(),
        repository_generation,
        freshness: Freshness::Current,
        index_scope: crate::model::IndexScopeMode::Full,
        index_scope_digest: None,
        source_tokens: emitted_tokens,
        protocol_tokens: 0,
        path_and_metadata_tokens: 0,
        total_response_tokens: 0,
        tokenizer: tokenizer.name().into(),
        token_count_exact: tokenizer.is_exact(),
        receipt_id: None,
        receipt_suppressed_exact: 0,
        receipt_suppressed_overlap: 0,
        receipt_near_duplicates: 0,
        next_cursor: None,
    };
    let mut response = ContextResponse {
        effective_response_profile: if request.explain_diagnostics {
            ContextResponseProfile::Explain
        } else {
            ContextResponseProfile::Balanced
        },
        workflow: crate::model::ContextWorkflow::Implementation,
        workflow_receipt: None,
        plan,
        fragments,
        receipt,
        diff_scope: None,
        omitted,
        omission_summary,
        coverage,
        routing: None,
        handoff_manifest: None,
        provenance: None,
        warnings,
        meta,
    };
    let accounting = tokens::response_token_accounting(&response, emitted_tokens, &tokenizer)
        .expect("context response metadata is serializable");
    response.meta.protocol_tokens = accounting.protocol_tokens;
    response.meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
    response.meta.total_response_tokens = accounting.total_response_tokens;
    response
}
