//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required revisions remain fixed across private replacements and undo.

use super::*;
use crate::mvcc::overlay::run::read_counts;

#[test]
fn repeated_requirements_do_not_read_spilled_private_records() {
    let control = StorageReadControl::with_limit(512 << 10);
    let mut transaction = transaction(&control);
    for id in 0..512_u64 {
        transaction
            .replace(&id.to_be_bytes(), Some(&[b'x'; 2048]), &control)
            .unwrap();
    }
    read_counts::take();
    transaction
        .require_unchanged(&0_u64.to_be_bytes(), &control)
        .unwrap();
    let first = read_counts::take();
    assert!(first.entries > 0, "the required key must have spilled");
    assert_eq!(first.values, 0, "requirements need no record payload");
    for _ in 0..128 {
        transaction
            .require_unchanged(&0_u64.to_be_bytes(), &control)
            .unwrap();
    }
    let reads = read_counts::take();
    assert_eq!(
        reads.entries, 0,
        "a repeated requirement needs no private record read"
    );
    assert_eq!(reads.values, 0);
    assert_eq!(transaction.requirements.len(), 1);

    // Model a graph's 64 endpoint records and 64 membership records, followed by 384 cyclic edges that each require both endpoints and memberships.
    for key in 0..128_u64 {
        transaction
            .require_unchanged(&key.to_be_bytes(), &control)
            .unwrap();
    }
    let initial = read_counts::take();
    assert!(initial.entries > 0, "the endpoint set must include spills");
    assert_eq!(initial.values, 0);
    for edge in 0..384_u64 {
        for endpoint in [edge % 64, (edge + 1) % 64] {
            for key in [endpoint, endpoint + 64] {
                transaction
                    .require_unchanged(&key.to_be_bytes(), &control)
                    .unwrap();
            }
        }
    }
    let repeated = read_counts::take();
    assert_eq!(repeated.entries, 0);
    assert_eq!(repeated.values, 0);
    assert_eq!(transaction.requirements.len(), 128);
    control.cancellation().cancel();
    assert!(transaction
        .require_unchanged(&0_u64.to_be_bytes(), &control)
        .is_err());
}

#[test]
fn observed_requirements_are_checked_against_the_selected_private_view_after_undo() {
    let control = StorageReadControl::with_limit(64 << 10);
    let mut transaction = transaction(&control);
    let key = RecordKey::new(b"external", control.memory()).unwrap();
    let expected = CommitSequence::from_u64(1);
    transaction
        .require_observed(&key, expected, &control)
        .unwrap();
    transaction
        .savepoint("before observed write", &control)
        .unwrap();
    assert!(matches!(
        transaction.require_unchanged(key.bytes(), &control),
        Err(VersionError::InvalidEncoding(
            "record requirements disagree"
        ))
    ));
    let mut value = BudgetedVec::new(control.memory());
    value.extend_from_slice(b"observed").unwrap();
    transaction
        .write_observed(&key, &Arc::new(value), expected, &control)
        .unwrap();
    transaction
        .require_unchanged(key.bytes(), &control)
        .unwrap();
    transaction
        .rollback_to("before observed write", &control)
        .unwrap();
    assert!(matches!(
        transaction.require_unchanged(key.bytes(), &control),
        Err(VersionError::InvalidEncoding(
            "record requirements disagree"
        ))
    ));
}

#[test]
fn private_replacements_preserve_required_revisions_and_reject_observed_disagreement() {
    let control = StorageReadControl::with_limit(64 << 10);
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let expected = store
        .commit(
            &[RecordWrite {
                key: b"existing",
                expected: None,
                value: Some(b"old"),
            }],
            &control,
        )
        .unwrap();
    let mut transaction =
        Transaction::at_snapshot(Arc::new(store.snapshot().unwrap()), false, &control);
    for key in [b"existing".as_slice(), b"new"] {
        transaction.require_unchanged(key, &control).unwrap();
        transaction.replace(key, Some(b"mine"), &control).unwrap();
        transaction.require_unchanged(key, &control).unwrap();
        transaction.replace(key, None, &control).unwrap();
        transaction.require_unchanged(key, &control).unwrap();
        transaction.replace(key, Some(b"final"), &control).unwrap();
        transaction.require_unchanged(key, &control).unwrap();
    }
    assert_eq!(transaction.requirements.len(), 2);
    assert_eq!(transaction.requirements[0].expected, Some(expected));
    assert_eq!(transaction.requirements[1].expected, None);
    store
        .commit(
            &[RecordWrite {
                key: b"new",
                expected: None,
                value: Some(b"peer"),
            }],
            &control,
        )
        .unwrap();
    assert!(matches!(
        store.commit_prepared(&transaction.prepare(&control).unwrap(), &control),
        Err(VersionError::WriteConflict {
            expected: None,
            actual: Some(_),
            ..
        })
    ));

    let mut other = super::transaction(&control);
    let key = RecordKey::new(b"new", control.memory()).unwrap();
    other.require_unchanged(key.bytes(), &control).unwrap();
    assert!(matches!(
        other.require_observed(&key, expected, &control),
        Err(VersionError::InvalidEncoding(
            "observed metadata preconditions disagree"
        ))
    ));
}

#[test]
fn unused_key_promises_do_not_hide_an_inconsistent_required_revision() {
    let control = StorageReadControl::with_limit(64 << 10);
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    store
        .commit(
            &[RecordWrite {
                key: b"existing",
                expected: None,
                value: Some(b"old"),
            }],
            &control,
        )
        .unwrap();
    let mut transaction =
        Transaction::at_snapshot(Arc::new(store.snapshot().unwrap()), false, &control);
    let key = RecordKey::new(b"existing", control.memory()).unwrap();
    transaction
        .require_unchanged(key.bytes(), &control)
        .unwrap();
    let mut value = BudgetedVec::new(control.memory());
    value.extend_from_slice(b"replacement").unwrap();
    transaction
        .write_unused_record(&key, &Arc::new(value), &control)
        .unwrap();
    assert!(matches!(
        transaction.require_unchanged(key.bytes(), &control),
        Err(VersionError::InvalidEncoding(
            "record requirements disagree"
        ))
    ));
}

#[test]
fn requirement_undo_releases_index_nodes_and_preserves_insertion_order() {
    let control = StorageReadControl::with_limit(64 << 10);
    let mut transaction = transaction(&control);
    transaction.require_unchanged(b"z", &control).unwrap();
    transaction.savepoint("prefix", &control).unwrap();
    transaction.require_unchanged(b"a", &control).unwrap();
    transaction.require_unchanged(b"z", &control).unwrap();
    transaction.rollback_to("prefix", &control).unwrap();
    transaction.release("prefix").unwrap();
    // The nonempty requirement vector may retain its capacity, but repeated undo must not accumulate removed keys or lookup nodes.
    let retained = control.memory().used();
    for key in 0..16_u8 {
        transaction.savepoint("prefix", &control).unwrap();
        transaction.require_unchanged(&[key], &control).unwrap();
        transaction.rollback_to("prefix", &control).unwrap();
        transaction.release("prefix").unwrap();
        assert_eq!(control.memory().used(), retained);
    }
    transaction.require_unchanged(b"b", &control).unwrap();
    let prepared = transaction.prepare(&control).unwrap();
    assert_eq!(prepared.required_keys().collect::<Vec<_>>(), [b"z", b"b"]);
    drop((prepared, transaction));
    assert_eq!(control.memory().used(), 0);
}
