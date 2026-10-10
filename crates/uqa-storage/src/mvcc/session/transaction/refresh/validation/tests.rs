//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{
    commit::RecordWriteKind,
    overlay::run::{read_counts, SpilledRunWriter},
    test_support::WindowSnapshot,
    CommitSequence, MemoryVersionStore, PrivateRecordRevision, RecordWrite,
};
use uqa_core::memory::MemoryBudget;

fn prepared(
    count: u64,
    sequence: CommitSequence,
    control: &StorageReadControl,
) -> PreparedRecordCommit {
    let mut spill = SpilledRunWriter::new(count, count * 8, control.memory()).unwrap();
    for id in 0..count {
        spill
            .push(
                &id.to_be_bytes(),
                matches!(id, 1 | 2).then_some(sequence),
                RecordWriteKind::Canonical,
                PrivateRecordRevision::for_tests(),
                (!id.is_multiple_of(3)).then_some(&[73; 1024][..]),
                control,
            )
            .unwrap();
    }
    PreparedRecordCommit::from_spilled_run(spill.finish().unwrap().unwrap(), control).unwrap()
}

fn store(control: &StorageReadControl) -> (MemoryVersionStore, CommitSequence) {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let sequence = store
        .commit(
            &[
                RecordWrite {
                    key: &1_u64.to_be_bytes(),
                    expected: None,
                    value: None,
                },
                RecordWrite {
                    key: &2_u64.to_be_bytes(),
                    expected: None,
                    value: Some(b"committed"),
                },
            ],
            control,
        )
        .unwrap();
    (store, sequence)
}

#[test]
fn refreshed_conditions_share_one_window_without_reading_values() {
    for count in [4, 128, 512] {
        let control = StorageReadControl::with_limit(128 << 10);
        let (store, sequence) = store(&control);
        let records = prepared(count, sequence, &control);
        let current = WindowSnapshot::new(store.snapshot().unwrap());
        store
            .commit(
                &[RecordWrite {
                    key: &0_u64.to_be_bytes(),
                    expected: None,
                    value: Some(b"newer than the selected snapshot"),
                }],
                &control,
            )
            .unwrap();
        read_counts::take();
        validate(&records, &current, &control).unwrap();
        current.assert_reads(1, 0);
        assert_eq!(current.requests(), usize::try_from(count).unwrap());
        assert_eq!(read_counts::take().values, 0);
        drop((current, records, store));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn refreshed_conditions_stop_at_the_first_conflict_or_cancellation() {
    for failing in [0_u64, 2, 127] {
        let control = StorageReadControl::with_limit(128 << 10);
        let (store, sequence) = store(&control);
        let records = prepared(128, sequence, &control);
        let expected = (failing == 2).then_some(sequence);
        let actual = store
            .commit(
                &[RecordWrite {
                    key: &failing.to_be_bytes(),
                    expected,
                    value: Some(b"peer replacement"),
                }],
                &control,
            )
            .unwrap();
        let current = WindowSnapshot::new(store.snapshot().unwrap());
        read_counts::take();
        assert!(matches!(
            validate(&records, &current, &control),
            Err(VersionError::WriteConflict { mutation, expected: found, actual: Some(revision) })
                if mutation == usize::try_from(failing).unwrap()
                    && found == expected && revision == actual
        ));
        current.assert_reads(1, 0);
        assert_eq!(current.requests(), usize::try_from(failing + 1).unwrap());
        assert_eq!(read_counts::take().values, 0);
        drop((current, records, store));
        assert_eq!(control.memory().used(), 0);
    }

    let control = StorageReadControl::with_limit(128 << 10);
    let (store, sequence) = store(&control);
    let records = prepared(128, sequence, &control);
    let current = WindowSnapshot::new(store.snapshot().unwrap()).cancelling_after(1);
    read_counts::take();
    assert!(matches!(
        validate(&records, &current, &control).map_err(VersionError::into_storage_error),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    current.assert_reads(1, 0);
    assert_eq!(current.requests(), 1);
    assert_eq!(read_counts::take().values, 0);
    drop((current, records, store));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn empty_refreshed_conditions_do_not_admit_a_read_window() {
    let control = StorageReadControl::with_limit(4096);
    let (store, _) = store(&control);
    let records = PreparedRecordCommit::new(&[], &control).unwrap();
    let current = WindowSnapshot::new(store.snapshot().unwrap());
    validate(&records, &current, &control).unwrap();
    current.assert_reads(0, 0);
    control.cancellation().cancel();
    assert!(matches!(
        validate(&records, &current, &control).map_err(VersionError::into_storage_error),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    current.assert_reads(0, 0);
    drop((current, records, store));
    assert_eq!(control.memory().used(), 0);
}
