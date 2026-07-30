use super::*;
pub(in crate::ranking) const FACET_PREFIX: &str = "facet:";
pub(in crate::ranking) const CHANNEL_PREFIX: &str = "channel:";
pub(in crate::ranking) const REQUIRED_EVIDENCE_PREFIX: &str = "required_evidence:";

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
