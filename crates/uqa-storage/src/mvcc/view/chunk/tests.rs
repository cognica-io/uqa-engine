//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{MemoryVersionStore, RecordWrite};
use uqa_core::memory::BudgetedVec;

#[test]
fn record_chunks_preserve_large_committed_values_across_later_writes() {
    let setup = StorageReadControl::with_limit(32 << 20);
    let store = MemoryVersionStore::new(setup.memory());
    let value: Vec<u8> = (0..(1 << 20) + 17).map(|i| (i % 251) as u8).collect();
    let sequence = store
        .commit(
            &[RecordWrite {
                key: b"large",
                expected: None,
                value: Some(&value),
            }],
            &setup,
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let control = StorageReadControl::with_limit(2048);
    let mut output = BudgetedVec::new(control.memory());
    output.extend_from_slice(&[0; 1024]).unwrap();
    let mut offset = 0;
    while offset < value.len() {
        let chunk = snapshot
            .read_value_chunk(b"large", offset, &mut output, &control)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.revision(), sequence);
        assert_eq!(chunk.total_bytes(), Some(value.len()));
        assert_eq!(chunk.copied(), output.len().min(value.len() - offset));
        assert_eq!(
            &output[..chunk.copied()],
            &value[offset..offset + chunk.copied()]
        );
        if offset == 0 {
            store
                .commit(
                    &[RecordWrite {
                        key: b"large",
                        expected: Some(sequence),
                        value: Some(b"new"),
                    }],
                    &setup,
                )
                .unwrap();
        }
        offset += chunk.copied();
    }
    assert_eq!(
        snapshot
            .read_value_chunk(b"large", offset, &mut output, &control)
            .unwrap()
            .unwrap()
            .copied(),
        0
    );
    assert!(snapshot
        .read_value_chunk(b"large", usize::MAX, &mut output, &control)
        .is_err());
    assert_eq!(control.memory().used(), output.capacity());
    assert!(control.memory().peak() <= 2048);
    drop(output);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn record_chunks_distinguish_empty_tombstone_absent_and_cancelled_reads() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    let revision = store
        .commit(
            &[
                RecordWrite {
                    key: b"empty",
                    expected: None,
                    value: Some(b""),
                },
                RecordWrite {
                    key: b"deleted",
                    expected: None,
                    value: None,
                },
                RecordWrite {
                    key: b"value",
                    expected: None,
                    value: Some(b"abcd"),
                },
            ],
            &control,
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let mut output = [0xcc; 8];
    for (key, length) in [(b"empty".as_slice(), Some(0)), (b"deleted", None)] {
        let chunk = snapshot
            .read_value_chunk(key, 0, &mut output, &control)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.revision(), revision);
        assert_eq!(chunk.total_bytes(), length);
        assert_eq!(chunk.copied(), 0);
        assert_eq!(output, [0xcc; 8]);
        assert!(snapshot
            .read_value_chunk(key, 1, &mut output, &control)
            .is_err());
    }
    assert!(snapshot
        .read_value_chunk(b"absent", 0, &mut output, &control)
        .unwrap()
        .is_none());
    assert_eq!(
        snapshot
            .read_value_chunk(b"value", 2, &mut [], &control)
            .unwrap()
            .unwrap()
            .total_bytes(),
        Some(4)
    );
    let chunk = snapshot
        .read_value_chunk(b"value", 2, &mut output, &control)
        .unwrap()
        .unwrap();
    assert_eq!(chunk.copied(), 2);
    assert_eq!(output, [b'c', b'd', 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc]);
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(matches!(
        snapshot.read_value_chunk(b"value", 0, &mut output, &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(output, [b'c', b'd', 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc]);
}
