//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::Value;

#[test]
fn returning_rows_spill_without_retaining_complete_output() {
    let directory = tempfile::tempdir().unwrap();
    let token = CancellationToken::new();
    let mut rows = ReturningRows::with_budget(64 * 1024, token);
    rows.buffer = SpillBuffer::new_in(32 * 1024, directory.path());
    let schema = RowSchema::new(vec!["item".into(), "item".into()]);
    let mut actual = Sha256::new();
    let allocations = allocation_counter::measure(|| {
        for index in 0..2048 {
            rows.push(OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_values(vec![Value::Int(index), Value::Str("x".repeat(16384))]),
            ))
            .unwrap();
        }
        assert_eq!(rows.len(), 2048);
        assert!(rows.buffer.has_spilled());
        let mut count = 0;
        for row in rows.into_rows().unwrap() {
            let row = row.unwrap();
            assert_eq!(row.schema.columns(), ["item", "item"]);
            assert_eq!(row.view().value_at(0), Some(&Value::Int(count)));
            let Some(Value::Str(body)) = row.view().value_at(1) else {
                panic!("RETURNING payload lost its position")
            };
            actual.update(body.as_bytes());
            count += 1;
        }
        assert_eq!(count, 2048);
    });
    let mut expected = Sha256::new();
    for _ in 0..2048 {
        expected.update([b'x'; 16384]);
    }
    assert_eq!(actual.finalize(), expected.finalize());
    // Two admitted batches, one incoming/decoded row and encoding scratch may coexist; 32 MiB of complete output may not.
    assert!(allocations.bytes_max < 1024 * 1024, "{allocations:?}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn returning_rows_detach_unselected_source_fragments() {
    let mut rows = ReturningRows::with_budget(64 * 1024, CancellationToken::new());
    let schema = RowSchema::new(vec!["id".into()]);
    let allocations = allocation_counter::measure(|| {
        for index in 0..64 {
            let source = Arc::new(vec![
                Value::Str("private".repeat(150_000)),
                Value::Int(index),
            ]);
            rows.push(OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_shared_values(source, Arc::from([1])),
            ))
            .unwrap();
        }
        assert_eq!(rows.len(), 64);
        for (index, row) in rows.into_rows().unwrap().enumerate() {
            assert_eq!(
                row.unwrap().view().value_at(0),
                Some(&Value::Int(index as i64))
            );
        }
    });
    // At most the current 1.05 MiB source and bounded selected IDs coexist, not 64 retained source documents.
    assert!(allocations.bytes_max < 3 * 1024 * 1024, "{allocations:?}");
}

#[test]
fn returning_rows_release_spills_after_cancellation_and_write_failure() {
    let directory = tempfile::tempdir().unwrap();
    let token = CancellationToken::new();
    let schema = RowSchema::new(vec!["body".into()]);
    let row = || {
        OwnedPhysicalRow::new(
            schema.clone(),
            PhysicalRow::from_values(vec![Value::Str("x".repeat(4096))]),
        )
    };
    let mut rows = ReturningRows::with_budget(1, token.clone());
    rows.buffer = SpillBuffer::new_in(0, directory.path());
    rows.push(row()).unwrap();
    let mut reader = rows.into_rows().unwrap();
    token.cancel();
    assert_eq!(
        reader.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(reader.next().is_none());
    drop(reader);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);

    let invalid = directory.path().join("not-a-directory");
    std::fs::write(&invalid, b"preserve").unwrap();
    let mut rows = ReturningRows::with_budget(1, CancellationToken::new());
    rows.buffer = SpillBuffer::new_in(0, &invalid);
    assert!(rows.push(row()).is_err());
    drop(rows);
    assert_eq!(std::fs::read(&invalid).unwrap(), b"preserve");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}
