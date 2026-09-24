//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn unpinned_replacements_reclaim_obsolete_payloads_before_the_transaction_ends() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut payload = vec![0; 256 << 10];
    for revision in 1..=12 {
        payload.fill(revision);
        changes.apply(&[write(b"row", &payload)], &control).unwrap();
        let snapshot = changes.snapshot().unwrap();
        let current = snapshot.get(b"row", &control).unwrap().unwrap();
        assert_eq!(current.value(), Some(payload.as_slice()));
    }
    let prepared = changes.prepare(&control).unwrap();
    assert_eq!(prepared.records().len(), 1);
    assert_eq!(prepared.records()[0].value(), Some(payload.as_slice()));
    drop(prepared);
    drop(changes);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_views_pin_only_their_own_values_through_replacements_and_owner_drop() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let empty = changes.snapshot().unwrap();
    let empty_bytes = control.memory().used();
    let mut payload = vec![1; 256 << 10];
    changes.apply(&[write(b"row", &payload)], &control).unwrap();
    let first = changes.snapshot().unwrap();
    for revision in 2..=12 {
        payload.fill(revision);
        changes.apply(&[write(b"row", &payload)], &control).unwrap();
        assert!(first
            .get(b"row", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap()
            .iter()
            .all(|byte| *byte == 1));
    }
    drop(changes);
    assert!(empty.get(b"row", &control).unwrap().is_none());
    let record = first.get(b"row", &control).unwrap().unwrap();
    drop(first);
    assert_eq!(record.value().unwrap().len(), 256 << 10);
    assert!(record.value().unwrap().iter().all(|byte| *byte == 1));
    drop(record);
    assert_eq!(control.memory().used(), empty_bytes);
    drop(empty);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn releasing_the_last_savepoint_reclaims_its_obsolete_values_immediately() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut payload = vec![1; 256 << 10];
    changes.apply(&[write(b"row", &payload)], &control).unwrap();
    let checkpoint = StorageSavepointId::allocate();
    changes.savepoint(checkpoint).unwrap();
    payload.fill(2);
    changes.apply(&[write(b"row", &payload)], &control).unwrap();
    let before_release = control.memory().used();
    changes.release_savepoint(checkpoint).unwrap();
    assert!(control.memory().used() <= before_release - payload.len());
    for revision in 3..=12 {
        payload.fill(revision);
        changes.apply(&[write(b"row", &payload)], &control).unwrap();
    }
    let final_records = changes.prepare(&control).unwrap();
    changes.rollback().unwrap();
    assert_eq!(final_records.records()[0].value(), Some(payload.as_slice()));
    drop(final_records);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn exhausted_allowance_rollback_frees_nested_savepoint_and_current_roots() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut payload = vec![1; 256 << 10];
    changes.apply(&[write(b"row", &payload)], &control).unwrap();
    let outer = StorageSavepointId::allocate();
    changes.savepoint(outer).unwrap();
    payload.fill(2);
    changes.apply(&[write(b"row", &payload)], &control).unwrap();
    let inner = StorageSavepointId::allocate();
    changes.savepoint(inner).unwrap();
    payload.fill(3);
    changes.apply(&[write(b"row", &payload)], &control).unwrap();
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    changes.rollback_to_savepoint(outer).unwrap();
    assert!(control.memory().used() <= control.memory().limit() - 2 * payload.len());
    assert!(matches!(
        changes.release_savepoint(inner),
        Err(VersionError::SavepointMissing(_))
    ));
    let restored = changes.snapshot().unwrap();
    assert!(restored
        .get(b"row", &control)
        .unwrap()
        .unwrap()
        .value()
        .unwrap()
        .iter()
        .all(|byte| *byte == 1));
    drop(restored);
    changes.rollback().unwrap();
    drop(held);
    assert_eq!(control.memory().used(), 0);
    assert!(!changes.has_written());
}
