//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::LockedRows;
use uqa_core::CancellationToken;

#[test]
fn update_locked_rows_bound_retention_and_preserve_first_target_selection() {
    const COUNT: u64 = 16_384;
    const TABLE: &str = "public.update_rows_with_rechecked_partition_keys";
    let allocated = allocation_counter::measure(|| {
        let mut rows = LockedRows::new(64 * 1024, CancellationToken::new(), "UPDATE locked rows");
        for id in (0..COUNT).rev() {
            assert!(rows.insert_if_absent(TABLE, id * 2).unwrap());
        }
        for id in (0..COUNT).step_by(37) {
            assert!(!rows.insert_if_absent(TABLE, id * 2).unwrap());
        }
        for (table, id) in [
            (TABLE, u64::MAX),
            ("public.other_partition", 0),
            ("public.other_partition", u64::MAX),
            ("", u64::MAX),
        ] {
            assert!(rows.insert_if_absent(table, id).unwrap());
            assert!(!rows.insert_if_absent(table, id).unwrap());
        }
    });
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn update_locked_rows_keep_cancellation_and_typed_memory_errors() {
    let cancel = CancellationToken::new();
    let mut rows = LockedRows::new(64 * 1024, cancel.clone(), "UPDATE locked rows");
    assert!(rows.insert_if_absent("t", 1).unwrap());
    cancel.cancel();
    assert_eq!(
        rows.insert_if_absent("t", 1).unwrap_err().sqlstate(),
        Some("57014")
    );
    let mut rows = LockedRows::new(0, CancellationToken::new(), "UPDATE locked rows");
    assert!(rows.insert_if_absent("t", 1).unwrap());
    assert!(!rows.insert_if_absent("t", 1).unwrap());
    // Zero work memory forces spill; the fixed hard workspace still rejects an oversized physical key.
    let oversized_relation = "t".repeat(64 * 1024);
    assert_eq!(
        rows.insert_if_absent(&oversized_relation, 2)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(!rows.insert_if_absent("t", 1).unwrap());
    assert!(rows.insert_if_absent("t", 2).unwrap());
}
