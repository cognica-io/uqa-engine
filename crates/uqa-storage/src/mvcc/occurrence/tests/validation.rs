//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::test_support::WindowSnapshot;

#[test]
fn occurrence_validation_closes_its_window_before_derived_reads() {
    for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
        let control = StorageReadControl::with_limit(1 << 20);
        let store = MemoryVersionStore::new(control.memory());
        let base = store.snapshot().unwrap();
        let current = WindowSnapshot::new(store.snapshot().unwrap());
        let keys = keys(&control);
        let values = payload();
        let derived = prepare(&keys, &values, None, &control);
        let mut writes = BudgetedVec::new(control.memory());
        for key in [&[0][..], &[255][..]] {
            writes
                .push(
                    PreparedRecordWrite::copy_bytes(key, None, Some(b"canonical"), &control)
                        .unwrap(),
                )
                .unwrap();
        }
        for write in derived.resident().unwrap() {
            writes.push(write.clone()).unwrap();
        }
        writes.sort_unstable_by(|left, right| left.key().cmp(right.key()));
        let original = PreparedRecordCommit::from_unique_owned(writes, &control).unwrap();
        let resolved = resolve(
            &original,
            &base,
            &current,
            &crate::key_value::KeyValueOccurrenceRecords,
            mode,
            &control,
        )
        .unwrap();
        // Both canonical ends validate in command mode; derived reads between them must run after the first physical window closes.
        current.assert_reads(
            if mode == ResolutionMode::Command {
                2
            } else {
                0
            },
            1,
        );
        assert_eq!(
            current.requests(),
            if mode == ResolutionMode::Command {
                2
            } else {
                0
            }
        );
        let rows = resolved.resident().unwrap();
        assert_eq!(rows.len(), 5);
        for (key, expected) in keys.iter().zip(&values) {
            let row = rows.iter().find(|write| write.key() == key).unwrap();
            assert_eq!(row.value(), Some(expected.as_slice()));
            assert_eq!(row.expected(), None);
            assert!(row.kind() == mode.kind(RecordWriteKind::Occurrence));
        }
        for key in [&[0][..], &[255][..]] {
            let row = rows.iter().find(|write| write.key() == key).unwrap();
            assert_eq!(row.value(), Some(b"canonical".as_slice()));
            assert!(row.kind() == RecordWriteKind::Canonical);
        }
        drop((resolved, original, derived, current, base, store));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn occurrence_validation_preserves_first_error_and_releases_admission() {
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(64 << 10);
        let store = MemoryVersionStore::new(control.memory());
        let base = store.snapshot().unwrap();
        let sequence = store
            .commit(
                &[RecordWrite {
                    key: &[1],
                    expected: None,
                    value: Some(b"peer"),
                }],
                &control,
            )
            .unwrap();
        let current = WindowSnapshot::new(store.snapshot().unwrap());
        let current = if cancel {
            current.cancelling_after(1)
        } else {
            current
        };
        let mut writes = BudgetedVec::new(control.memory());
        for (key, kind) in [
            (&[0][..], RecordWriteKind::Canonical),
            (&[1][..], RecordWriteKind::Canonical),
            (&[255][..], RecordWriteKind::Occurrence),
        ] {
            writes
                .push(
                    PreparedRecordWrite::copy_bytes(key, None, Some(b"evaluated"), &control)
                        .unwrap()
                        .with_kind(kind),
                )
                .unwrap();
        }
        let original = PreparedRecordCommit::from_unique_owned(writes, &control).unwrap();
        let baseline = control.memory().used();
        let result = resolve(
            &original,
            &base,
            &current,
            &crate::key_value::KeyValueOccurrenceRecords,
            ResolutionMode::Command,
            &control,
        );
        if cancel {
            assert!(matches!(result, Err(VersionError::Cancelled(_))));
        } else {
            assert!(matches!(result, Err(VersionError::WriteConflict {
                mutation: 1, expected: None, actual: Some(actual),
            }) if actual == sequence));
        }
        current.assert_reads(1, 0);
        assert_eq!(current.requests(), if cancel { 1 } else { 2 });
        assert_eq!(control.memory().used(), baseline);
        assert_eq!(original.resident().unwrap().len(), 3);
        drop((original, current, base, store));
        assert_eq!(control.memory().used(), 0);
    }
}
