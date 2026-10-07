use super::*;

pub(super) fn request_with_budget(budget: usize) -> ContextRequest {
    ContextRequest {
        task: "rank source evidence for a task".into(),
        token_budget: budget,
        include_paths: Vec::new(),
        must_include_paths: Vec::new(),
        must_include_symbols: Vec::new(),
        required_evidence: Vec::new(),
        max_fragments: None,
        plan_only: false,
        focus_paths: Vec::new(),
        strict_focus_paths: false,
        minimum_fragments_per_focus_path: None,
        focus_symbols: Vec::new(),
        exclude_paths: Vec::new(),
        known_hashes: Vec::new(),
        receipt_id: None,
        prior_repository_generation: None,
        base_revision: None,
        changed_paths: Vec::new(),
        strict_changed_paths: false,
        explain_diagnostics: false,
    }
}

pub(super) fn request_focused(budget: usize, focus_path: &str) -> ContextRequest {
    let mut request = request_with_budget(budget);
    request.focus_paths = vec![focus_path.into()];
    request
}
