fn build_context_plan(
    request: &ContextRequest,
    selected: &[ScoredCandidate],
    candidate_paths_total: usize,
    estimated_source_tokens: usize,
    generated_artifact_warning: bool,
    result_complete: bool,
) -> Option<ContextQueryPlan> {
    request.plan_only.then(|| {
        let minimum_fragments = request.minimum_fragments_per_focus_path.unwrap_or(1);
        let focus_coverage = request
            .focus_paths
            .iter()
            .map(|pattern| {
                let matcher = PathMatcher::new_lossy(std::slice::from_ref(pattern));
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
            .map(|scored| ContextPlanCandidate {
                path: scored.candidate.path.clone(),
                start_line: scored.candidate.start_line,
                end_line: scored.candidate.end_line,
                target_start_line: scored.candidate.target_start_line,
                target_end_line: scored.candidate.target_end_line,
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

fn materialize_context_fragments(
    request: &ContextRequest,
    selected: &[ScoredCandidate],
    estimated_source_tokens: usize,
) -> (Vec<ContextFragment>, Vec<String>, usize) {
    if request.plan_only {
        return (Vec::new(), Vec::new(), 0);
    }
    let fragments = selected
        .iter()
        .map(|scored| ContextFragment {
            path: scored.candidate.path.clone(),
            start_line: scored.candidate.start_line,
            end_line: scored.candidate.end_line,
            target_start_line: scored.candidate.target_start_line,
            target_end_line: scored.candidate.target_end_line,
            truncated: scored.candidate.target_truncated(),
            representation: scored.candidate.representation.clone(),
            content: scored.candidate.content.clone(),
            content_hash: scored.content_hash.clone(),
            score: (scored.score * 10_000.0).round() / 10_000.0,
            reason: scored.candidate.reason(),
            token_count: scored.token_count,
        })
        .collect();
    let fragment_hashes = selected
        .iter()
        .map(|scored| scored.content_hash.clone())
        .collect();
    (fragments, fragment_hashes, estimated_source_tokens)
}

#[allow(clippy::too_many_arguments)]
fn finalize_context_response(
    request: &ContextRequest,
    repository_generation: u64,
    tokenizer: tokens::Tokenizer,
    plan: Option<ContextQueryPlan>,
    fragments: Vec<ContextFragment>,
    fragment_hashes: Vec<String>,
    emitted_tokens: usize,
    omitted: Vec<OmittedCandidate>,
    omission_summary: ContextOmissionSummary,
    coverage: ContextCoverageReceipt,
    warnings: Vec<String>,
) -> ContextResponse {
    let task_hash = blake3::hash(request.task.as_bytes()).to_hex().to_string();
    let receipt = EvidenceReceipt {
        task_fingerprint: task_hash[..32].to_string(),
        fragment_hashes,
    };
    let meta = ResponseMeta {
        repository_id: String::new(),
        repository_generation,
        freshness: Freshness::Current,
        source_tokens: emitted_tokens,
        protocol_tokens: 0,
        path_and_metadata_tokens: 0,
        total_response_tokens: 0,
        payload_tokens: 0,
        tokenizer: tokenizer.name().into(),
        emitted_tokens,
        token_count_exact: tokenizer.is_exact(),
        receipt_id: None,
        receipt_suppressed_exact: 0,
        receipt_suppressed_overlap: 0,
        receipt_near_duplicates: 0,
        next_cursor: None,
    };
    let mut response = ContextResponse {
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
        warnings,
        meta,
    };
    let accounting = tokens::response_token_accounting(&response, emitted_tokens, &tokenizer)
        .expect("context response metadata is serializable");
    response.meta.protocol_tokens = accounting.protocol_tokens;
    response.meta.path_and_metadata_tokens = accounting.path_and_metadata_tokens;
    response.meta.total_response_tokens = accounting.total_response_tokens;
    response.meta.payload_tokens = accounting.total_response_tokens;
    response
}
