//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::mvcc::{
    CommittedRecordSnapshot, MemoryVersionStore, PreparedRecordCommit, RecordWrite, VersionError,
};

use super::*;

fn obsolete(store: &MemoryVersionStore, control: &StorageReadControl) -> CommitSequence {
    let first = store
        .commit(
            &[RecordWrite {
                key: b"owned/key",
                expected: None,
                value: Some(b"first"),
            }],
            control,
        )
        .unwrap();
    let deleted = store
        .commit(
            &[RecordWrite {
                key: b"owned/key",
                expected: Some(first),
                value: None,
            }],
            control,
        )
        .unwrap();
    store.reclaim().unwrap();
    deleted
}

#[test]
fn tombstone_reclamation_fences_original_absence_without_changing_other_prefixes_or_visibility() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    let empty = store.snapshot().unwrap();
    let writes = [RecordWrite {
        key: b"owned/key",
        expected: None,
        value: Some(b"late"),
    }];
    let old = PreparedRecordCommit::new_at_snapshot(&writes, &empty, &control).unwrap();
    let raw = PreparedRecordCommit::new(&writes, &control).unwrap();
    assert_ne!(old.fingerprint(), raw.fingerprint());
    drop(empty);
    let through = obsolete(&store, &control);
    let request = TombstoneReclamationRequest {
        prefix: b"owned/",
        after: None,
        through,
    };
    let deleted = store.snapshot().unwrap();
    assert!(matches!(
        store.reclaim_tombstones(&request, &control).unwrap(),
        TombstoneReclamationStep::Retained
    ));
    assert_eq!(deleted.get(b"owned/key").unwrap().sequence(), through);
    drop(deleted);
    assert!(matches!(
        store.reclaim_tombstones(&request, &control).unwrap(),
        TombstoneReclamationStep::Complete { removed: 1 }
    ));
    let current = store.snapshot().unwrap();
    assert_eq!(current.sequence(), through);
    assert_eq!(current.reclamation_epoch(), Some(1));
    assert!(current.get(b"owned/key").is_none());
    for prepared in [&old, &raw] {
        assert!(matches!(
            store.commit_prepared(prepared, &control),
            Err(VersionError::ReclaimedObservation { minimum: 1, .. })
        ));
    }
    let resolved = PreparedRecordCommit::new(&writes, &control)
        .unwrap()
        .resolved(&old, through);
    assert_eq!(resolved.fingerprint(), old.fingerprint());
    assert!(matches!(
        store.commit_prepared(&resolved, &control),
        Err(VersionError::ReclaimedObservation {
            observed: Some(0),
            ..
        })
    ));
    let fresh = PreparedRecordCommit::new_at_snapshot(&writes, &current, &control).unwrap();
    let published = store.commit_prepared(&fresh, &control).unwrap();
    assert_eq!(published, through.successor().unwrap());
    store
        .commit(
            &[
                RecordWrite {
                    key: b"outside/new",
                    expected: None,
                    value: Some(b"unchanged contract"),
                },
                RecordWrite {
                    key: b"owned/key",
                    expected: Some(published),
                    value: Some(b"observed revision"),
                },
            ],
            &control,
        )
        .unwrap();
}

#[test]
fn tombstone_reclamation_checks_read_only_absence_requirements_and_preserves_known_revisions() {
    use crate::mvcc::commit::RecordRequirement;
    use crate::mvcc::key::RecordKey;
    let control = StorageReadControl::with_limit(1 << 20);
    let key = RecordKey::new(b"owned/key", control.memory()).unwrap();
    let requirement = [RecordRequirement {
        key,
        expected: None,
    }];
    let prepared = PreparedRecordCommit::new(&[], &control)
        .unwrap()
        .with_requirements(&requirement, &control)
        .unwrap()
        .with_reclamation_epoch(Some(3));
    assert!(matches!(
        prepared.validate_reclamation_epoch(b"owned/", 4, 4, control.cancellation()),
        Err(VersionError::ReclaimedObservation { .. })
    ));
    prepared
        .validate_reclamation_epoch(b"outside/", 4, 4, control.cancellation())
        .unwrap();
    for epoch in [None, Some(2), Some(5)] {
        let changed = PreparedRecordCommit::new(&[], &control)
            .unwrap()
            .with_requirements(&requirement, &control)
            .unwrap()
            .with_reclamation_epoch(epoch);
        assert!(changed
            .validate_reclamation_epoch(b"owned/", 3, 4, control.cancellation())
            .is_err());
    }
    prepared
        .validate_reclamation_epoch(b"owned/", 3, 4, control.cancellation())
        .unwrap();
}

#[test]
fn tombstone_reclamation_is_finite_across_later_deletes_and_recreated_keys() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    let keys: Vec<_> = (0..70_u8)
        .map(|suffix| [b"owned/".as_slice(), &[suffix]].concat())
        .collect();
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"first"),
        })
        .collect();
    let first = store.commit(&writes, &control).unwrap();
    let deletes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: Some(first),
            value: None,
        })
        .collect();
    let through = store.commit(&deletes, &control).unwrap();
    store.reclaim().unwrap();
    let mut request = TombstoneReclamationRequest {
        prefix: b"owned/",
        after: None,
        through,
    };
    let TombstoneReclamationStep::More { after, removed } =
        store.reclaim_tombstones(&request, &control).unwrap()
    else {
        panic!("first page must retain its cursor")
    };
    assert_eq!(removed, 64);
    let snapshot = store.snapshot().unwrap();
    let writes = [
        RecordWrite {
            key: &keys[0],
            expected: None,
            value: Some(b"earlier"),
        },
        RecordWrite {
            key: &keys[69],
            expected: Some(through),
            value: Some(b"recreated"),
        },
        RecordWrite {
            key: b"owned/\xff",
            expected: None,
            value: Some(b"later"),
        },
    ];
    let prepared = PreparedRecordCommit::new_at_snapshot(&writes, &snapshot, &control).unwrap();
    let published = store.commit_prepared(&prepared, &control).unwrap();
    drop(snapshot);
    let later = store
        .commit(
            &[
                RecordWrite {
                    key: &keys[0],
                    expected: Some(published),
                    value: None,
                },
                RecordWrite {
                    key: b"owned/\xff",
                    expected: Some(published),
                    value: None,
                },
            ],
            &control,
        )
        .unwrap();
    request.after = Some(&after);
    assert!(matches!(
        store.reclaim_tombstones(&request, &control).unwrap(),
        TombstoneReclamationStep::Complete { removed: 5 }
    ));
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.sequence(), later);
    assert_eq!(snapshot.reclamation_epoch(), Some(2));
    assert!(snapshot.get(&keys[64]).is_none());
    assert_eq!(snapshot.get(&keys[0]).unwrap().sequence(), later);
    assert_eq!(snapshot.get(b"owned/\xff").unwrap().sequence(), later);
    drop(snapshot);
    request.after = None;
    request.through = later;
    store.reclaim().unwrap();
    assert!(matches!(
        store.reclaim_tombstones(&request, &control).unwrap(),
        TombstoneReclamationStep::Complete { removed: 2 }
    ));
}

#[test]
fn tombstone_reclamation_rejects_resources_and_invalid_ranges_before_metadata_changes() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    let through = obsolete(&store, &control);
    let request = TombstoneReclamationRequest {
        prefix: b"owned/",
        after: None,
        through,
    };
    assert!(store
        .reclaim_tombstones(&request, &StorageReadControl::with_limit(0))
        .is_err());
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(store.reclaim_tombstones(&request, &cancelled).is_err());
    let invalid = TombstoneReclamationRequest {
        after: Some(b"another/key"),
        ..request
    };
    assert!(store.reclaim_tombstones(&invalid, &control).is_err());
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.reclamation_epoch(), Some(0));
    assert_eq!(snapshot.get(b"owned/key").unwrap().sequence(), through);
}
