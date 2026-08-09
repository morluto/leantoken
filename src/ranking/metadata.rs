use super::*;
pub(in crate::ranking) const FACET_PREFIX: &str = "facet:";
pub(in crate::ranking) const CHANNEL_PREFIX: &str = "channel:";
pub(in crate::ranking) const REQUIRED_EVIDENCE_PREFIX: &str = "required_evidence:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextPathClass {
    Production,
    Test,
    Auxiliary,
    Supporting,
}

pub(crate) fn required_evidence_marker(requirement: usize, query: usize) -> String {
    format!("{REQUIRED_EVIDENCE_PREFIX}{requirement}:{query}")
}

pub(in crate::ranking) fn required_evidence_query(
    candidate: &Candidate,
    requirement: usize,
    query: usize,
) -> bool {
    let marker = required_evidence_marker(requirement, query);
    candidate.match_kinds.iter().any(|kind| kind == &marker)
}

pub(in crate::ranking) fn carries_required_evidence(
    candidate: &Candidate,
    requirement: usize,
) -> bool {
    let prefix = format!("{REQUIRED_EVIDENCE_PREFIX}{requirement}:");
    candidate
        .match_kinds
        .iter()
        .any(|kind| kind.starts_with(&prefix))
}

pub(in crate::ranking) fn carries_facet(candidate: &Candidate, facet: &str) -> bool {
    let prefix = format!("{FACET_PREFIX}{facet}:");
    candidate
        .match_kinds
        .iter()
        .any(|kind| kind.starts_with(&prefix))
}

pub(crate) fn context_path_class(path: &str) -> ContextPathClass {
    let path = path.to_ascii_lowercase();
    let root_markdown = !path.contains('/')
        && (path.ends_with(".md") || path.ends_with(".mdx"))
        && !matches!(path.as_str(), "readme.md" | "agents.md" | "contributing.md");
    if path.starts_with(".agents/")
        || path.starts_with("fixtures/")
        || path.starts_with("benchmarks/reports/")
        || path.contains("/snapshots/")
        || path.ends_with(".snap")
        || root_markdown
        || path.contains("/requests/")
        || path.contains("/schema/")
    {
        ContextPathClass::Auxiliary
    } else if path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("/test-suite/")
        || path.starts_with("crates/test-suite/")
        || path.ends_with("/test.rs")
        || path.ends_with("/tests.rs")
        || path.ends_with("_test.rs")
        || path.contains("/test_")
    {
        ContextPathClass::Test
    } else if path.starts_with("src/") || (path.starts_with("crates/") && path.contains("/src/")) {
        ContextPathClass::Production
    } else {
        ContextPathClass::Supporting
    }
}

pub(crate) fn owner_path_prior(path: &str, call_edge_intent: bool) -> f64 {
    let base = match context_path_class(path) {
        ContextPathClass::Production => 4.0,
        ContextPathClass::Test => 0.5,
        ContextPathClass::Auxiliary => -8.0,
        ContextPathClass::Supporting => 0.0,
    };
    let owner = if path.starts_with("src/services/") {
        3.0
    } else if path == "src/main/dispatch.rs" || path.contains("/dispatch.rs") {
        2.5
    } else {
        0.0
    };
    let call_edge = if call_edge_intent
        && (path.contains("dispatch")
            || path.contains("execution")
            || path.contains("pipeline")
            || path.contains("services"))
    {
        2.0
    } else {
        0.0
    };
    base + owner + call_edge
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_path_classes_separate_production_tests_and_auxiliary_evidence() {
        assert_eq!(
            context_path_class("src/services/context.rs"),
            ContextPathClass::Production
        );
        assert_eq!(
            context_path_class("crates/test-suite/src/domains/retrieval.rs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/services/context/tests.rs"),
            ContextPathClass::Test
        );
        assert_eq!(
            context_path_class("src/mcp/snapshots/tools.snap"),
            ContextPathClass::Auxiliary
        );
        assert_eq!(
            context_path_class("retrieval_notes.md"),
            ContextPathClass::Auxiliary
        );
    }
}
