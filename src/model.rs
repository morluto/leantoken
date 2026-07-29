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
pub use savings::*;
pub use search::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consistency_names_are_explicit_and_legacy_inputs_remain_readable() {
        assert_eq!(
            serde_json::to_string(&IndexConsistency::IndexedGeneration)
                .expect("serialize indexed generation"),
            "\"indexed_generation\""
        );
        assert_eq!(
            serde_json::to_string(&IndexConsistency::ReconcileWorkingTree)
                .expect("serialize working-tree reconciliation"),
            "\"reconcile_working_tree\""
        );
        assert_eq!(
            serde_json::from_str::<IndexConsistency>("\"committed\"")
                .expect("legacy committed alias"),
            IndexConsistency::IndexedGeneration
        );
        assert_eq!(
            serde_json::from_str::<IndexConsistency>("\"working_tree\"")
                .expect("legacy working-tree alias"),
            IndexConsistency::ReconcileWorkingTree
        );
    }

    #[test]
    fn focus_coverage_keeps_legacy_results_readable_and_bounds_diagnostics() {
        let legacy: ContextFocusPathCoverage = serde_json::from_value(serde_json::json!({
            "pattern": "src/**",
            "indexed_paths": 2,
            "minimum_fragments": 1,
            "selected_fragments": 1,
            "satisfied": true
        }))
        .expect("deserialize legacy focus coverage");
        assert_eq!(legacy.diagnostics, None);

        let current = ContextFocusPathCoverage {
            pattern: "src/**".into(),
            indexed_paths: 2,
            minimum_fragments: 2,
            selected_fragments: 1,
            satisfied: false,
            diagnostics: Some(ContextFocusPathDiagnostics {
                eligible_paths: 2,
                generated_fragments: 3,
                generated_symbol_fragments: 2,
                reserved_fragments: 1,
                selected_source_tokens: 40,
                suppressed_by: vec![ContextFocusSuppression {
                    boundary: ContextFocusSuppressionBoundary::MaxFragments,
                    fragments: 2,
                }],
                capacity_blocker: Some(ContextFocusCapacityBlocker::MaxFragments),
            }),
        };
        assert_eq!(
            serde_json::to_value(current).expect("serialize focus diagnostics"),
            serde_json::json!({
                "pattern": "src/**",
                "indexed_paths": 2,
                "minimum_fragments": 2,
                "selected_fragments": 1,
                "satisfied": false,
                "diagnostics": {
                    "eligible_paths": 2,
                    "generated_fragments": 3,
                    "generated_symbol_fragments": 2,
                    "reserved_fragments": 1,
                    "selected_source_tokens": 40,
                    "suppressed_by": [{
                        "boundary": "max_fragments",
                        "fragments": 2
                    }],
                    "capacity_blocker": "max_fragments"
                }
            })
        );
    }

    #[test]
    fn index_report_preserves_unknown_legacy_skip_reasons_and_serializes_known_counts() {
        let legacy: IndexReport = serde_json::from_value(serde_json::json!({
            "repository_generation": 1,
            "files_seen": 2,
            "files_indexed": 1,
            "files_unchanged": 0,
            "files_removed": 0,
            "files_skipped": 1,
            "warnings": []
        }))
        .expect("deserialize legacy index report");
        assert_eq!(legacy.skip_reasons, None);
        let legacy_value = serde_json::to_value(&legacy).expect("reserialize legacy report");
        assert!(legacy_value.get("skip_reasons").is_none());

        let skip_reasons = IndexSkipReasonCounts {
            binary: 1,
            oversized_during_read: 2,
            failed: 3,
        };
        let response = IndexResponse {
            repository_generation: 2,
            files_seen: 7,
            files_indexed: 1,
            files_unchanged: 0,
            files_removed: 2,
            files_skipped: skip_reasons.total(),
            warnings: vec!["failed preparation".into()],
        };
        let report = IndexReport::with_skip_reasons(response, skip_reasons);
        let value = serde_json::to_value(report).expect("serialize index report");

        assert_eq!(value["files_skipped"], 6);
        assert_eq!(
            value["skip_reasons"],
            serde_json::json!({
                "binary": 1,
                "oversized_during_read": 2,
                "failed": 3
            })
        );
        let round_trip: IndexReport =
            serde_json::from_value(value).expect("deserialize current index report");
        assert_eq!(
            round_trip.skip_reasons,
            Some(IndexSkipReasonCounts {
                binary: 1,
                oversized_during_read: 2,
                failed: 3,
            })
        );
        assert_eq!(round_trip.files_skipped, 6);
    }

    #[test]
    fn status_response_serializes_readiness_independently_from_freshness() {
        for (repository_generation, index_state, freshness) in [
            (0, IndexState::Uninitialized, Freshness::Current),
            (0, IndexState::Uninitialized, Freshness::Reconciling),
            (4, IndexState::Ready, Freshness::Current),
            (4, IndexState::Ready, Freshness::Reconciling),
        ] {
            let response = StatusResponse {
                repository_root: "/repository".into(),
                database_path: "/cache/index.sqlite".into(),
                index_content_version: 12,
                repository_generation,
                index_state,
                working_tree_checked: false,
                freshness: freshness.clone(),
                file_count: 0,
                chunk_count: 0,
                symbol_count: 0,
                index_storage_bytes: 0,
                indexed_source_bytes: 0,
                index_amplification_ratio: None,
                process_rss_bytes: None,
                index_progress: None,
                languages: Vec::new(),
                warnings: Vec::new(),
            };

            let value = serde_json::to_value(response).expect("serialize status");
            assert_eq!(value["index_content_version"], 12);
            assert_eq!(
                value["index_state"],
                match index_state {
                    IndexState::Uninitialized => "uninitialized",
                    IndexState::Ready => "ready",
                }
            );
            assert_eq!(
                value["freshness"],
                match freshness {
                    Freshness::Current => "current",
                    Freshness::Reconciling => "reconciling",
                }
            );
            assert_eq!(value["working_tree_checked"], false);
        }
    }

    #[test]
    fn compact_context_response_round_trips_with_defaults() {
        let response = ContextResponse {
            workflow: ContextWorkflow::Implementation,
            workflow_receipt: None,
            plan: None,
            effective_response_profile: ContextResponseProfile::Balanced,
            fragments: vec![ContextFragment {
                path: "src/lib.rs".into(),
                start_line: 1,
                end_line: 2,
                target_start_line: None,
                target_end_line: None,
                truncated: false,
                representation: "source".into(),
                content: "fn answer() {}".into(),
                content_hash: "receipt-hash".into(),
                score: 2.0,
                reason: "symbol".into(),
                token_count: 4,
            }],
            receipt: EvidenceReceipt {
                task_fingerprint: "task".into(),
                fragment_hashes: vec!["receipt-hash".into()],
            },
            diff_scope: None,
            omitted: Vec::new(),
            omission_summary: ContextOmissionSummary::default(),
            coverage: ContextCoverageReceipt::default(),
            routing: None,
            handoff_manifest: None,
            warnings: Vec::new(),
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Current,
                source_tokens: 4,
                protocol_tokens: 0,
                path_and_metadata_tokens: 0,
                total_response_tokens: 0,
                payload_tokens: 0,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 4,
                token_count_exact: true,
                receipt_id: None,
                receipt_suppressed_exact: 0,
                receipt_suppressed_overlap: 0,
                receipt_near_duplicates: 0,
                next_cursor: None,
            },
        };

        let value = serde_json::to_value(&response).expect("serialize response");
        assert!(value["fragments"][0].get("representation").is_none());
        assert!(value["fragments"][0].get("content_hash").is_none());
        assert!(value["receipt"].get("task_fingerprint").is_none());
        assert_eq!(value["meta"]["freshness"], "current");
        assert_eq!(value["meta"]["source_tokens"], 4);
        assert_eq!(value["meta"]["tokenizer"], "cl100k_base");
        assert_eq!(value["meta"]["token_count_exact"], true);
        assert!(value.get("omitted").is_none());
        assert!(value.get("warnings").is_none());

        let round_trip: ContextResponse =
            serde_json::from_value(value).expect("deserialize compact response");
        assert_eq!(round_trip.fragments[0].representation, "source");
        assert_eq!(round_trip.fragments[0].content_hash, "");
        assert!(round_trip.receipt.task_fingerprint.is_empty());
        assert_eq!(round_trip.meta.freshness, Freshness::Current);
        assert_eq!(round_trip.meta.source_tokens, 4);
        assert_eq!(round_trip.meta.tokenizer, "cl100k_base");
        assert!(round_trip.meta.token_count_exact);

        let mut legacy_value = serde_json::to_value(response).expect("serialize legacy response");
        let legacy_meta = legacy_value["meta"]
            .as_object_mut()
            .expect("response metadata object");
        legacy_meta.remove("source_tokens");
        legacy_meta.remove("protocol_tokens");
        legacy_meta.remove("path_and_metadata_tokens");
        legacy_meta.remove("total_response_tokens");
        legacy_meta.remove("payload_tokens");
        legacy_meta.remove("tokenizer");
        let legacy: ContextResponse =
            serde_json::from_value(legacy_value).expect("deserialize legacy response");
        assert_eq!(legacy.meta.source_tokens, 0);
        assert_eq!(legacy.meta.protocol_tokens, 0);
        assert_eq!(legacy.meta.path_and_metadata_tokens, 0);
        assert_eq!(legacy.meta.total_response_tokens, 0);
        assert_eq!(legacy.meta.payload_tokens, 0);
        assert!(legacy.meta.tokenizer.is_empty());
    }

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
            warnings: vec!["1 omitted".into()],
            meta: ResponseMeta {
                repository_id: "repository".into(),
                repository_generation: 7,
                freshness: Freshness::Reconciling,
                source_tokens: 9,
                protocol_tokens: 17,
                path_and_metadata_tokens: 97,
                total_response_tokens: 123,
                payload_tokens: 123,
                tokenizer: "cl100k_base".into(),
                emitted_tokens: 9,
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

    #[test]
    fn compact_empty_outline_round_trips_with_defaults() {
        let file = OutlineFile {
            path: "README.md".into(),
            language: None,
            parse_complete: true,
            structurally_complete: true,
            symbols: Vec::new(),
            imports: Vec::new(),
        };

        let value = serde_json::to_value(&file).expect("serialize outline");
        assert!(value.get("symbols").is_none());
        assert!(value.get("imports").is_none());

        let round_trip: OutlineFile =
            serde_json::from_value(value).expect("deserialize compact outline");
        assert!(round_trip.symbols.is_empty());
        assert!(round_trip.imports.is_empty());
    }
}
