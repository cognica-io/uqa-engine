//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private mutation visibility, retained command sources and savepoint isolation.

use std::collections::BTreeMap;

use proptest::prelude::*;
use uqa_core::memory::{MemoryBudget, MemoryError};
use uqa_storage::mvcc::{
    CommitSequence, MemoryVersionStore, PreparedRecordWrite, PrivateRecordChanges,
    PrivateRecordSnapshot, RecordWrite, VersionError,
};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::StorageSavepointId;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

fn write<'a>(key: &'a [u8], value: &'a [u8]) -> RecordWrite<'a> {
    RecordWrite {
        key,
        expected: None,
        value: Some(value),
    }
}

fn value(snapshot: &PrivateRecordSnapshot, key: &[u8]) -> Option<Vec<u8>> {
    snapshot
        .get(key, &control())
        .unwrap()
        .and_then(|write| write.value().map(<[u8]>::to_vec))
}

#[test]
fn private_key_revisions_survive_undo_without_reuse_or_payload_hydration() {
    let storage = StorageReadControl::with_limit(4 << 20);
    let read = StorageReadControl::with_limit(4096);
    let a = PrivateRecordChanges::new(storage.memory());
    let b = PrivateRecordChanges::new(storage.memory());
    let payload = vec![7; 1 << 20];
    a.apply(&[write(b"a", &payload)], &storage).unwrap();
    let original = a.snapshot().unwrap();
    let first = original.scan_keys(b"", None, 1, &read).unwrap()[0].revision();
    let checkpoint = StorageSavepointId::allocate();
    a.savepoint(checkpoint).unwrap();
    a.apply(
        &[RecordWrite {
            key: b"a",
            expected: None,
            value: None,
        }],
        &storage,
    )
    .unwrap();
    let deleted = a.snapshot().unwrap();
    let second = deleted.scan_keys(b"", None, 1, &read).unwrap()[0].revision();
    assert_ne!(first, second);
    a.rollback_to_savepoint(checkpoint).unwrap();
    assert_eq!(
        a.snapshot()
            .unwrap()
            .scan_keys(b"", None, 1, &read)
            .unwrap()[0]
            .revision(),
        first
    );
    a.apply(&[write(b"b", b"next")], &storage).unwrap();
    b.apply(&[write(b"a", b"other transaction")], &storage)
        .unwrap();
    let last = a
        .snapshot()
        .unwrap()
        .scan_keys(b"", Some(b"a"), 1, &read)
        .unwrap();
    assert_eq!(last[0].key(), b"b");
    let other = b
        .snapshot()
        .unwrap()
        .scan_keys(b"a", None, 1, &read)
        .unwrap();
    assert!(first < second && second < last[0].revision());
    assert_ne!(last[0].revision(), other[0].revision());
    a.rollback().unwrap();
    assert!(a
        .snapshot()
        .unwrap()
        .scan_keys(b"", None, 8, &read)
        .unwrap()
        .is_empty());
    assert_eq!(
        original.scan_keys(b"", None, 1, &read).unwrap()[0].revision(),
        first
    );
    assert_eq!(
        deleted.scan_keys(b"", None, 1, &read).unwrap()[0].revision(),
        second
    );
    assert!(original.scan_keys(b"z", None, 1, &read).unwrap().is_empty());
    assert!(original.scan_keys(b"", None, 0, &read).unwrap().is_empty());
    assert!(original
        .scan_keys(b"", None, 1, &StorageReadControl::with_limit(0))
        .is_err());
    let cancelled = StorageReadControl::with_limit(4096);
    cancelled.cancellation().cancel();
    assert!(original.scan_keys(b"", None, 1, &cancelled).is_err());
}

#[test]
fn command_views_keep_values_across_later_changes_and_rollback() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    let empty = changes.snapshot().unwrap();
    changes.apply(&[write(b"a", b"first")], &control).unwrap();
    let first = changes.snapshot().unwrap();
    let keep = StorageSavepointId::allocate();
    changes.savepoint(keep).unwrap();
    changes
        .apply(
            &[write(b"a", b"second"), write(b"b", b"inserted")],
            &control,
        )
        .unwrap();
    let second = changes.snapshot().unwrap();
    changes.rollback_to_savepoint(keep).unwrap();
    let restored = changes.snapshot().unwrap();
    changes
        .apply(
            &[write(b"a", b"third"), write(b"b", b"replacement")],
            &control,
        )
        .unwrap();
    assert!(value(&empty, b"a").is_none());
    assert_eq!(value(&first, b"a").unwrap(), b"first");
    assert_eq!(value(&second, b"a").unwrap(), b"second");
    assert_eq!(value(&second, b"b").unwrap(), b"inserted");
    assert_eq!(value(&restored, b"a").unwrap(), b"first");
    assert!(restored.get(b"b", &control).unwrap().is_none());
    assert_eq!(value(&changes.snapshot().unwrap(), b"a").unwrap(), b"third");
    changes.rollback().unwrap();
    assert!(!changes.has_written());
    assert!(changes.prepare(&control).unwrap().records().is_empty());
    drop(changes);
    assert_eq!(value(&second, b"b").unwrap(), b"inserted");
}

#[test]
fn nested_and_duplicate_savepoints_restore_only_the_nearest_scope() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    let outer = StorageSavepointId::allocate();
    let duplicate = StorageSavepointId::allocate();
    changes.savepoint(outer).unwrap();
    changes.apply(&[write(b"a", b"one")], &control).unwrap();
    changes.savepoint(duplicate).unwrap();
    changes.apply(&[write(b"a", b"two")], &control).unwrap();
    changes.savepoint(duplicate).unwrap();
    changes.apply(&[write(b"a", b"three")], &control).unwrap();
    changes.rollback_to_savepoint(duplicate).unwrap();
    assert_eq!(value(&changes.snapshot().unwrap(), b"a").unwrap(), b"two");
    changes.apply(&[write(b"a", b"again")], &control).unwrap();
    changes.rollback_to_savepoint(duplicate).unwrap();
    assert_eq!(value(&changes.snapshot().unwrap(), b"a").unwrap(), b"two");
    changes.release_savepoint(duplicate).unwrap();
    changes.rollback_to_savepoint(duplicate).unwrap();
    assert_eq!(value(&changes.snapshot().unwrap(), b"a").unwrap(), b"one");
    changes.release_savepoint(outer).unwrap();
    assert!(changes.has_written());
    assert!(matches!(
        changes.rollback_to_savepoint(duplicate),
        Err(VersionError::SavepointMissing(_))
    ));
    assert!(matches!(
        changes.release_savepoint(outer),
        Err(VersionError::SavepointMissing(_))
    ));
}

#[test]
fn rollback_to_an_empty_scope_restores_written_state_without_allocating() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    let empty = StorageSavepointId::allocate();
    changes.savepoint(empty).unwrap();
    changes.apply(&[write(b"a", b"one")], &control).unwrap();
    let retained = changes.snapshot().unwrap();
    let used = control.memory().used();
    let reserve = control
        .memory()
        .reserve(control.memory().limit() - used)
        .unwrap();
    changes.rollback_to_savepoint(empty).unwrap();
    assert!(!changes.has_written());
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(reserve);
    assert_eq!(value(&retained, b"a").unwrap(), b"one");
    changes.apply(&[write(b"b", b"two")], &control).unwrap();
    changes.rollback_to_savepoint(empty).unwrap();
    assert!(!changes.has_written());
}

#[test]
fn failed_savepoint_creation_does_not_install_a_checkpoint() {
    let memory = MemoryBudget::new(0);
    let changes = PrivateRecordChanges::new(&memory);
    let checkpoint = StorageSavepointId::allocate();
    assert!(matches!(
        changes.savepoint(checkpoint),
        Err(VersionError::Memory(_))
    ));
    assert!(matches!(
        changes.rollback_to_savepoint(checkpoint),
        Err(VersionError::SavepointMissing(_))
    ));
}

#[test]
fn final_replacements_preserve_original_revisions_and_tombstones() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    let revision = Some(CommitSequence::from_u64(5));
    changes
        .apply(
            &[RecordWrite {
                key: b"a",
                expected: revision,
                value: Some(b"first"),
            }],
            &control,
        )
        .unwrap();
    changes
        .apply(
            &[RecordWrite {
                key: b"a",
                expected: revision,
                value: None,
            }],
            &control,
        )
        .unwrap();
    let deleted = changes.snapshot().unwrap();
    assert!(deleted
        .get(b"a", &control)
        .unwrap()
        .unwrap()
        .value()
        .is_none());
    assert!(deleted.get(b"missing", &control).unwrap().is_none());
    changes
        .apply(&[write(b"z", b"last"), write(b"b", b"middle")], &control)
        .unwrap();
    let final_changes = changes.prepare(&control).unwrap();
    let records = final_changes.records();
    assert_eq!(
        records
            .iter()
            .map(PreparedRecordWrite::key)
            .collect::<Vec<_>>(),
        [b"a", b"b", b"z"]
    );
    assert_eq!(records[0].expected(), revision);
    assert!(records[0].value().is_none());
    changes.rollback().unwrap();
    assert_eq!(records[2].value().unwrap(), b"last");
}

#[test]
fn inconsistent_base_revision_rejects_the_whole_private_batch() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    changes
        .apply(&[write(b"z", b"original")], &control)
        .unwrap();
    let result = changes.apply(
        &[
            write(b"a", b"must not appear"),
            RecordWrite {
                key: b"z",
                expected: Some(CommitSequence::from_u64(3)),
                value: None,
            },
        ],
        &control,
    );
    assert!(matches!(
        result,
        Err(VersionError::WriteConflict { mutation: 1, .. })
    ));
    let view = changes.snapshot().unwrap();
    assert!(view.get(b"a", &control).unwrap().is_none());
    assert_eq!(value(&view, b"z").unwrap(), b"original");
}

#[test]
fn pinned_private_pages_ignore_later_inserts_and_retain_deleted_entries() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    changes
        .apply(
            &[
                write(b"r/a", b"a"),
                write(b"r/c", b"c"),
                write(b"other", b"other"),
            ],
            &control,
        )
        .unwrap();
    let view = changes.snapshot().unwrap();
    let first = view.scan(b"r/", None, 1, &control).unwrap();
    changes
        .apply(
            &[
                write(b"r/b", b"b"),
                RecordWrite {
                    key: b"r/c",
                    expected: None,
                    value: None,
                },
            ],
            &control,
        )
        .unwrap();
    let next = view.scan(b"r/", Some(first[0].key()), 2, &control).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].key(), b"r/c");
    assert_eq!(next[0].value().unwrap(), b"c");
    let current = changes
        .snapshot()
        .unwrap()
        .scan(b"r/", None, 10, &control)
        .unwrap();
    assert_eq!(current.len(), 3);
    assert!(current[2].value().is_none());
    assert!(view
        .scan(b"r/", Some(b"s/"), 2, &control)
        .unwrap()
        .is_empty());
    assert_eq!(
        view.scan(b"r/", Some(b"a"), 1, &control).unwrap()[0].key(),
        b"r/a"
    );
    let no_memory = StorageReadControl::with_limit(0);
    assert!(view.scan(b"r/", None, 0, &no_memory).unwrap().is_empty());
    assert!(matches!(
        view.scan(b"r/", None, 1, &no_memory),
        Err(VersionError::Memory(_))
    ));
}

#[test]
fn private_failure_and_cancellation_leave_prior_changes_available() {
    let control = control();
    let changes = PrivateRecordChanges::new(control.memory());
    changes
        .apply(&[write(b"a", b"original")], &control)
        .unwrap();
    assert!(matches!(
        changes.apply(&[write(b"a", b"one"), write(b"a", b"two")], &control),
        Err(VersionError::DuplicateRecord { .. })
    ));
    let view = changes.snapshot().unwrap();
    control.cancellation().cancel();
    assert!(matches!(
        changes.apply(&[write(b"a", b"cancelled")], &control),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        changes.prepare(&control),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        view.get(b"a", &control),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        view.scan(b"", None, 1, &control),
        Err(VersionError::Cancelled(_))
    ));
    control.cancellation().reset();
    assert_eq!(
        value(&changes.snapshot().unwrap(), b"a").unwrap(),
        b"original"
    );
}

#[test]
fn bounded_preparation_failures_publish_no_partial_private_batch() {
    let mut failures = 0;
    let mut successes = 0;
    for allowance in (0..4096).step_by(32) {
        let memory = MemoryBudget::new(16384);
        let control = StorageReadControl::new(&memory, &uqa_core::CancellationToken::new());
        let changes = PrivateRecordChanges::new(&memory);
        changes
            .apply(&[write(b"z", b"original")], &control)
            .unwrap();
        let used = memory.used();
        let held = memory.reserve(memory.limit() - used - allowance).unwrap();
        let result = changes.apply(
            &[write(b"a", b"first"), write(b"z", b"replacement")],
            &control,
        );
        drop(held);
        let view = changes.snapshot().unwrap();
        match result {
            Ok(()) => {
                successes += 1;
                assert_eq!(value(&view, b"a").unwrap(), b"first");
                assert_eq!(value(&view, b"z").unwrap(), b"replacement");
            }
            Err(VersionError::Memory(MemoryError::Limit { .. })) => {
                failures += 1;
                assert!(view.get(b"a", &control).unwrap().is_none());
                assert_eq!(value(&view, b"z").unwrap(), b"original");
            }
            Err(error) => panic!("unexpected staging result: {error}"),
        }
        drop(view);
        drop(changes);
        assert_eq!(memory.used(), 0);
    }
    assert!(failures > 1 && successes > 1);
}

#[test]
fn independent_commit_survives_another_owners_savepoint_and_full_rollback() {
    let control = control();
    let store = MemoryVersionStore::new(control.memory());
    let a = PrivateRecordChanges::new(control.memory());
    let b = PrivateRecordChanges::new(control.memory());
    a.apply(&[write(b"a", b"before")], &control).unwrap();
    let keep = StorageSavepointId::allocate();
    a.savepoint(keep).unwrap();
    a.apply(&[write(b"a", b"later")], &control).unwrap();
    b.apply(&[write(b"b", b"independent")], &control).unwrap();
    store
        .commit_prepared(&b.prepare(&control).unwrap(), &control)
        .unwrap();
    a.rollback_to_savepoint(keep).unwrap();
    store
        .commit_prepared(&a.prepare(&control).unwrap(), &control)
        .unwrap();
    a.rollback().unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(&***snapshot.get(b"a").unwrap().value().unwrap(), b"before");
    assert_eq!(
        &***snapshot.get(b"b").unwrap().value().unwrap(),
        b"independent"
    );
}

proptest! {
    #[test]
    fn retained_views_match_a_copied_model_across_savepoint_branches(
        operations in prop::collection::vec((0_u8..8, 0_u8..6, any::<u8>()), 1..96)
    ) {
        let control = control();
        let changes = PrivateRecordChanges::new(control.memory());
        let ids = [StorageSavepointId::allocate(), StorageSavepointId::allocate(), StorageSavepointId::allocate()];
        let mut current: BTreeMap<u8, Option<u8>> = BTreeMap::new();
        let mut checkpoints = Vec::new();
        let mut snapshots = Vec::new();
        for (operation, key, byte) in operations {
            let id = ids[usize::from(key) % ids.len()];
            match operation {
                0 | 1 => {
                    let value = (operation == 0).then_some(byte);
                    changes.apply(&[RecordWrite { key: &[key], expected: None, value: value.as_ref().map(std::slice::from_ref) }], &control).unwrap();
                    current.insert(key, value);
                }
                2 => {
                    changes.savepoint(id).unwrap();
                    checkpoints.push((id, current.clone()));
                }
                3 => {
                    let result = changes.rollback_to_savepoint(id);
                    if let Some(position) = checkpoints.iter().rposition(|(saved, _)| *saved == id) {
                        result.unwrap();
                        current.clone_from(&checkpoints[position].1);
                        checkpoints.truncate(position + 1);
                    } else {
                        prop_assert!(matches!(result, Err(VersionError::SavepointMissing(_))));
                    }
                }
                4 => {
                    let result = changes.release_savepoint(id);
                    if let Some(position) = checkpoints.iter().rposition(|(saved, _)| *saved == id) {
                        result.unwrap();
                        checkpoints.truncate(position);
                    } else {
                        prop_assert!(matches!(result, Err(VersionError::SavepointMissing(_))));
                    }
                }
                5 => {
                    changes.rollback().unwrap();
                    current.clear();
                    checkpoints.clear();
                }
                _ => snapshots.push((changes.snapshot().unwrap(), current.clone())),
            }
            prop_assert_eq!(changes.has_written(), !current.is_empty());
        }
        snapshots.push((changes.snapshot().unwrap(), current));
        drop(changes);
        for (snapshot, expected) in snapshots {
            for key in 0_u8..6 {
                let actual = snapshot.get(&[key], &control).unwrap().map(|write| write.value().map(|value| value[0]));
                prop_assert_eq!(actual, expected.get(&key).copied());
            }
            let page = snapshot.scan(b"", None, 10, &control).unwrap();
            let actual: BTreeMap<_, _> = page.iter().map(|write| (write.key()[0], write.value().map(|value| value[0]))).collect();
            prop_assert_eq!(actual, expected);
        }
        prop_assert_eq!(control.memory().used(), 0);
    }
}
