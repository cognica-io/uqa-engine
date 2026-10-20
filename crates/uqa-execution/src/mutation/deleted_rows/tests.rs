//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn deleted_row_membership_bounds_retention_and_preserves_exact_physical_keys() {
    const COUNT: u64 = 16_384;
    const TABLE: &str = "public.deleted_rows_with_a_reused_partition_key";
    let allocated = allocation_counter::measure(|| {
        let mut rows = DeletedRowBuffer::new(64 * 1024, CancellationToken::new());
        for id in (0..COUNT).rev() {
            assert!(rows.insert_if_absent(TABLE, id * 2).unwrap());
        }
        for id in [0, 2, COUNT, (COUNT - 1) * 2] {
            assert!(!rows.insert_if_absent(TABLE, id).unwrap());
        }
        for id in [0, 2, COUNT, (COUNT - 1) * 2, u64::MAX] {
            rows.insert(TABLE, id).unwrap();
        }
        rows.insert("public.other_partition", 3).unwrap();
        let rows = rows.finish().unwrap();
        for id in (0..COUNT).step_by(37) {
            assert!(rows.contains(TABLE, id * 2).unwrap());
            assert!(!rows.contains(TABLE, id * 2 + 1).unwrap());
            assert!(!rows.contains("public.other_partition", id * 2).unwrap());
        }
        assert!(rows.contains(TABLE, u64::MAX).unwrap());
        assert!(rows.contains("public.other_partition", 3).unwrap());
        assert!(!rows.contains(TABLE, COUNT * 2).unwrap());
        assert!(!rows.contains("public.absent_partition", u64::MAX).unwrap());
    });
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn deleted_row_membership_propagates_cancellation_before_write_and_read() {
    let cancel = CancellationToken::new();
    let mut buffer = DeletedRowBuffer::new(64 * 1024, cancel.clone());
    buffer.insert("t", 1).unwrap();
    let rows = buffer.finish().unwrap();
    let mut unfinished = DeletedRowBuffer::new(64 * 1024, cancel.clone());
    cancel.cancel();
    assert_eq!(rows.contains("t", 1).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(
        unfinished.insert("t", 2).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(
        unfinished.insert_if_absent("t", 2).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(unfinished.finish().err().unwrap().sqlstate(), Some("57014"));
}

#[test]
fn physical_row_membership_spills_below_one_key_without_losing_exact_keys() {
    for bytes in [0, 1, 80] {
        let allocated = allocation_counter::measure(|| {
            let mut rows = DeletedRowBuffer::new(bytes, CancellationToken::new());
            for id in (0..256).rev() {
                assert!(rows.insert_if_absent("partition", id).unwrap());
            }
            assert!(!rows.insert_if_absent("partition", 17).unwrap());
            rows.insert("partition2", DocId::MAX).unwrap();
            let rows = rows.finish().unwrap();
            for id in 0..256 {
                assert!(rows.contains("partition", id).unwrap());
                assert!(!rows.contains("partition2", id).unwrap());
            }
            assert!(rows.contains("partition2", DocId::MAX).unwrap());
        });
        assert!(allocated.bytes_max < 512 * 1024, "{bytes}: {allocated:?}");
    }
}
