use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod common;
mod context;
mod files;
mod history;
mod index;
mod json;
mod outline;
mod read;
mod receipt;
mod savings;
mod search;

pub use common::*;
pub use context::*;
pub use files::*;
pub use history::*;
pub use index::*;
pub use json::*;
pub use outline::*;
pub use read::*;
pub use receipt::*;
pub use savings::*;
pub use search::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_context_response_snapshot() {
        let response = ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            plan: None,
            effective_response_profile: ContextResponseProfile::Balanced,
            fragments: vec![ContextFragment {
                path: "src/lib.rs".into(),
                start_line: 4,
                end_line: 6,
                target_start_line: None,
                target_end_line: None,
                truncated: false,
                representation: "source".into(),
                content: "pub fn answer() -> u8 { 42 }".into(),
                content_hash: "fragment-hash".into(),
                score: 1.25,
                reason: "symbol; focus".into(),
                token_count: 9,
            }],
            receipt: EvidenceReceipt {
                task_fingerprint: "internal-task-fingerprint".into(),
                fragment_hashes: vec!["fragment-hash".into()],
            },
            diff_scope: None,
            omitted: vec![OmittedCandidate {
                path: "src/other.rs".into(),
                start_line: 10,
                end_line: 12,
                reason: "budget or result limit".into(),
            }],
            omission_summary: ContextOmissionSummary {
                budget_or_result_limit: 1,
                ..ContextOmissionSummary::default()
            },
            coverage: ContextCoverageReceipt::default(),
            routing: None,
            handoff_manifest: None,
            provenance: None,
            warnings: vec!["1 omitted".into()],
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Reconciling,
                index_scope: IndexScopeMode::Full,
                index_scope_digest: None,
                source_tokens: 9,
                protocol_tokens: 17,
                path_and_metadata_tokens: 97,
                total_response_tokens: 123,
                tokenizer: "cl100k_base".into(),
                token_count_exact: true,
                receipt_id: None,
                receipt_suppressed_exact: 0,
                receipt_suppressed_overlap: 0,
                receipt_near_duplicates: 0,
                next_cursor: None,
            },
        };

        insta::assert_json_snapshot!(response);
    }
}
