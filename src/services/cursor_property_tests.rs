//! Property-based contract tests for the unified cursor primitive.
//!
//! These tests verify universal properties that hold for all cursor states,
//! not just handpicked examples. They use `proptest` to generate random valid
//! cursor states across all operations and verify round-trip, mutation, and
//! tamper-resistance laws.
//!
//! See issue #524: Add model- and property-based contract tests for cursors,
//! pagination, and request state spaces.

use proptest::prelude::*;

use crate::services::cursor::{binding_hash, decode_cursor, encode_cursor};

proptest! {
    /// Law 1: Round-trip — for any valid cursor, decode(encode(cursor)) == cursor.
    #[test]
    fn cursor_round_trip_preserves_offset(
        kind in "[a-z]{1,10}",
        generation in 0u64..100_000u64,
        field1 in "[a-z0-9]{1,16}",
        field2 in "[a-z0-9]{1,16}",
        offset in 0usize..10_000,
    ) {
        let binding = binding_hash(&[&field1, &field2]);
        let cursor = encode_cursor(&kind, generation, &binding, offset);
        let decoded = decode_cursor(&cursor, &kind, generation, &binding)
            .expect("round-trip decode should succeed");
        prop_assert_eq!(decoded, offset);
    }

    /// Law 2: Wrong kind is always rejected.
    #[test]
    fn cursor_wrong_kind_rejected(
        kind in "[a-z]{1,10}",
        wrong_kind in "[a-z]{1,10}",
        generation in 0u64..100_000u64,
        field1 in "[a-z0-9]{1,16}",
        offset in 0usize..10_000,
    ) {
        prop_assume!(kind != wrong_kind);
        let binding = binding_hash(&[&field1]);
        let cursor = encode_cursor(&kind, generation, &binding, offset);
        prop_assert!(
            decode_cursor(&cursor, &wrong_kind, generation, &binding).is_err(),
            "cursor with wrong kind should be rejected"
        );
    }

    /// Law 3: Wrong generation is always rejected.
    #[test]
    fn cursor_wrong_generation_rejected(
        kind in "[a-z]{1,10}",
        generation in 0u64..100_000u64,
        wrong_generation in 0u64..100_000u64,
        field1 in "[a-z0-9]{1,16}",
        offset in 0usize..10_000,
    ) {
        prop_assume!(generation != wrong_generation);
        let binding = binding_hash(&[&field1]);
        let cursor = encode_cursor(&kind, generation, &binding, offset);
        prop_assert!(
            decode_cursor(&cursor, &kind, wrong_generation, &binding).is_err(),
            "cursor with wrong generation should be rejected"
        );
    }

    /// Law 4: Wrong binding hash is always rejected.
    #[test]
    fn cursor_wrong_binding_rejected(
        kind in "[a-z]{1,10}",
        generation in 0u64..100_000u64,
        field1 in "[a-z0-9]{1,16}",
        field2 in "[a-z0-9]{1,16}",
        offset in 0usize..10_000,
    ) {
        prop_assume!(field1 != field2);
        let binding = binding_hash(&[&field1]);
        let wrong_binding = binding_hash(&[&field2]);
        let cursor = encode_cursor(&kind, generation, &binding, offset);
        prop_assert!(
            decode_cursor(&cursor, &kind, generation, &wrong_binding).is_err(),
            "cursor with wrong binding should be rejected"
        );
    }

    /// Law 5: Tampered offset is always rejected by the MAC.
    #[test]
    fn cursor_tampered_offset_rejected(
        kind in "[a-z]{1,10}",
        generation in 0u64..100_000u64,
        field1 in "[a-z0-9]{1,16}",
        offset in 1usize..10_000,
        tampered_offset in 0usize..10_000,
    ) {
        prop_assume!(offset != tampered_offset);
        let binding = binding_hash(&[&field1]);
        let cursor = encode_cursor(&kind, generation, &binding, offset);
        let parts: Vec<&str> = cursor.split(':').collect();
        prop_assume!(parts.len() == 5);
        let tampered = format!("{}:{}:{}:{}:{}",
            parts[0], parts[1], parts[2], tampered_offset, parts[4]);
        prop_assert!(
            decode_cursor(&tampered, &kind, generation, &binding).is_err(),
            "tampered cursor should be rejected"
        );
    }

    /// Law 6: Truncated cursor is always rejected.
    #[test]
    fn cursor_truncated_rejected(
        kind in "[a-z]{1,10}",
        generation in 0u64..100_000u64,
        field1 in "[a-z0-9]{1,16}",
        offset in 0usize..10_000,
    ) {
        let binding = binding_hash(&[&field1]);
        let cursor = encode_cursor(&kind, generation, &binding, offset);
        let truncated = &cursor[..cursor.len() - 1];
        prop_assert!(
            decode_cursor(truncated, &kind, generation, &binding).is_err(),
            "truncated cursor should be rejected"
        );
    }
}
