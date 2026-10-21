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
        let mut rows = PhysicalRowBuffer::new(
            64 * 1024,
            CancellationToken::new(),
            "deleted-row membership",
        );
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
    let mut buffer = PhysicalRowBuffer::new(64 * 1024, cancel.clone(), "deleted-row membership");
    buffer.insert("t", 1).unwrap();
    let rows = buffer.finish().unwrap();
    let mut unfinished =
        PhysicalRowBuffer::new(64 * 1024, cancel.clone(), "deleted-row membership");
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
fn deleted_row_membership_preserves_typed_memory_exhaustion() {
    let memory = MemoryBudget::new(0);
    let mut rows = PhysicalRowBuffer {
        records: PrivateRecordChanges::new(&memory),
        control: StorageReadControl::new(&memory, &CancellationToken::new()),
        operation: "deleted-row membership",
    };
    let error = rows.insert("t", 1).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{error:?}");
    let error = rows.insert_if_absent("t", 1).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{error:?}");
}

#[test]
fn physical_row_membership_spills_below_one_key_without_losing_exact_keys() {
    for bytes in [0, 1, 80] {
        let allocated = allocation_counter::measure(|| {
            let mut rows =
                PhysicalRowBuffer::new(bytes, CancellationToken::new(), "row membership");
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

#[test]
fn physical_row_replay_preserves_relation_and_unsigned_identity_order() {
    let tables = ["", "a", "a\0", "a\0b", "aa", "b", "한국어"];
    let ids = [0, 1, 255, 256, u64::MAX];
    let expected: Vec<_> = tables
        .iter()
        .flat_map(|table| ids.map(|id| ((*table).to_owned(), id)))
        .collect();
    for allowance in [0, 64 * 1024] {
        let mut rows = PhysicalRowBuffer::new(allowance, CancellationToken::new(), "MERGE targets");
        for (table, id) in expected.iter().rev() {
            rows.insert(table, *id).unwrap();
            assert!(rows.snapshot().unwrap().contains(table, *id).unwrap());
        }
        let rows = rows.finish().unwrap();
        let mut actual = Vec::new();
        rows.visit(&mut |table, id| {
            actual.push((table.to_owned(), id));
            Ok(true)
        })
        .unwrap();
        assert_eq!(actual, expected);
        for (table, id) in &actual {
            assert!(rows.contains(table, *id).unwrap());
        }
    }
}
