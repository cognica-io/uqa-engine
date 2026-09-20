//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained views and authoritative receipts determine which participants can be recovered.

use uqa_storage::mvcc::{SerializableParticipant, StorageTransactionId};

use super::*;

#[test]
fn retained_read_only_completion_survives_an_independent_owners_recovery() {
    use uqa_storage::mvcc::{SerializableCoordinator, SerializableStatus};

    for mode in 0..5 {
        let (_directory, a, b) = owners(mode);
        let control = control();
        let (completed, view) = a.admit_serializable_snapshot(true, &control).unwrap();
        a.with_serializable_admission(&control, &mut |graph, _| {
            graph.prepare_commit(completed.id(), &control)?;
            graph.commit(completed.id())
        })
        .unwrap();
        let next = admit(&b, &control);
        b.with_serializable_admission(&control, &mut |graph, _| {
            assert_eq!(graph.status(completed.id())?, SerializableStatus::Committed);
            assert!(graph.publication(completed.id())?.is_none());
            graph.commit(completed.id())
        })
        .unwrap();
        assert_eq!(b.allocate_transaction(&control).unwrap().allocation(), 1);
        drop((completed, view, next));
        b.recover_serializable(&control).unwrap();
        assert_eq!(control.memory().used(), 0, "mode {mode}");
    }
}

fn owners(mode: usize) -> (tempfile::TempDir, SQLiteRecordStore, SQLiteRecordStore) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("participants.db");
    let first = if mode == 4 {
        ManagedConnection::open_in_memory().unwrap()
    } else {
        open(&path, mode)
    };
    let a = SQLiteRecordStore::new(&first).unwrap();
    let second = if mode == 4 {
        first.new_session()
    } else {
        open(&path, mode)
    };
    let b = SQLiteRecordStore::new(&second).unwrap();
    (directory, a, b)
}

pub(super) fn admit(
    store: &SQLiteRecordStore,
    control: &StorageReadControl,
) -> SerializableParticipant {
    store
        .admit_serializable(false, control, || Ok(()))
        .unwrap()
        .0
}

pub(super) fn prepare(
    store: &SQLiteRecordStore,
    actor: &SerializableParticipant,
    key: &[u8],
    control: &StorageReadControl,
) -> (StorageTransactionId, PreparedRecordCommit) {
    let transaction = store.allocate_transaction(control).unwrap();
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key,
            expected: None,
            value: Some(b"durable"),
        }],
        control,
    )
    .unwrap();
    let mut held = store.serializable_admission(control).unwrap();
    held.graph_mut()
        .prepare_publication(actor.id(), transaction, prepared.fingerprint(), control)
        .unwrap();
    held.persist(control).unwrap();
    (transaction, prepared)
}

#[test]
fn cloned_participants_survive_independent_owners_and_only_dead_handles_are_reclaimed() {
    for mode in 0..5 {
        let (_directory, a, b) = owners(mode);
        let control = control();
        let provider: &dyn VersionedPersistence = &a;
        let (actor, view) = provider
            .serializable_coordinator()
            .unwrap()
            .admit_serializable_snapshot(false, &control)
            .unwrap();
        let id = actor.id();
        let nested = actor.clone();
        let peer = admit(&b, &control);
        let peer_id = peer.id();
        // The original connection and handle may close while a nested view retains this actor.
        drop((a, actor));
        b.recover_serializable(&control).unwrap();
        let held = b.serializable_admission(&control).unwrap();
        held.graph().check_active(id).unwrap();
        held.graph().check_active(peer_id).unwrap();
        // Releasing a participant and its snapshot must not acquire SSI admission.
        drop((nested, view));
        drop(held);
        b.recover_serializable(&control).unwrap();
        let held = b.serializable_admission(&control).unwrap();
        assert!(held.graph().check_active(id).is_err());
        held.graph().check_active(peer_id).unwrap();
        drop(held);
        let next = admit(&b, &control);
        assert!(next.id().allocation() > peer_id.allocation());
        assert_eq!(next.id().coordinator(), id.coordinator());
        drop((peer, next));
        b.recover_serializable(&control).unwrap();
        assert_eq!(control.memory().used(), 0, "mode {mode}");
    }
}

#[test]
fn abandoned_publishers_resolve_their_physical_outcome_before_the_next_snapshot() {
    for mode in 0..5 {
        let (_directory, a, b) = owners(mode);
        let control = control();
        let survivor = admit(&b, &control);
        let pending = admit(&a, &control);
        let committed = admit(&a, &control);
        let (pending_id, pending_write) = prepare(&a, &pending, b"pending", &control);
        let (committed_id, committed_write) = prepare(&a, &committed, b"committed", &control);
        b.recover_serializable(&control).unwrap();
        assert_eq!(
            b.commit_status(pending_id, &control).unwrap(),
            CommitStatus::Pending
        );
        let held = a.serializable_admission(&control).unwrap();
        let publication = held.graph().publication(committed.id()).unwrap().unwrap();
        let receipt = a.commit(committed_id, &committed_write, &control).unwrap();
        // The physical COMMIT succeeds while the corresponding graph completion is lost.
        drop((held, a, pending, committed, pending_write, committed_write));
        let (later, view) = b
            .admit_serializable(true, &control, || {
                assert_eq!(
                    b.commit_status(pending_id, &control).unwrap(),
                    CommitStatus::Aborted
                );
                b.snapshot(&control)
            })
            .unwrap();
        assert!(view.get(b"pending", &control).unwrap().is_none());
        let record = view.get(b"committed", &control).unwrap().unwrap();
        assert_eq!(record.value().map(|value| &***value), Some(&b"durable"[..]));
        drop(record);
        let mut held = b.serializable_admission(&control).unwrap();
        held.graph().check_active(survivor.id()).unwrap();
        assert_eq!(
            held.graph_mut()
                .resolve_publication(publication, CommitStatus::Unknown)
                .unwrap(),
            CommitStatus::Committed(receipt)
        );
        drop((held, survivor, later, view));
        b.recover_serializable(&control).unwrap();
        assert_eq!(control.memory().used(), 0, "mode {mode}");
    }
}

#[test]
fn failed_or_cancelled_snapshot_capture_consumes_admission_without_replaying_it() {
    for mode in 0..5 {
        let (_directory, a, b) = owners(mode);
        let control = control();
        let survivor = admit(&a, &control);
        let mut calls = 0;
        for cancel in [false, true] {
            let result = a.admit_serializable::<()>(false, &control, || {
                calls += 1;
                if cancel {
                    control.cancellation().cancel();
                    control.cancellation().check()?;
                }
                Err(VersionError::InvalidEncoding("capture failed"))
            });
            assert!(if cancel {
                matches!(result, Err(VersionError::Cancelled(_)))
            } else {
                matches!(result, Err(VersionError::InvalidEncoding("capture failed")))
            });
            control.cancellation().reset();
        }
        assert_eq!(calls, 2);
        let next = admit(&b, &control);
        assert_eq!(next.id().allocation(), survivor.id().allocation() + 3);
        let held = b.serializable_admission(&control).unwrap();
        held.graph().check_active(survivor.id()).unwrap();
        drop((held, survivor, next));
        b.recover_serializable(&control).unwrap();
        assert_eq!(control.memory().used(), 0, "mode {mode}");
    }
}

#[test]
fn unknown_receipts_stop_snapshot_capture_without_discarding_confirmed_outcomes() {
    let (_directory, a, b) = owners(4);
    let control = control();
    let survivor = admit(&b, &control);
    let committed = admit(&a, &control);
    let unknown = admit(&a, &control);
    let (physical, prepared) = prepare(&a, &committed, b"committed", &control);
    let missing = StorageTransactionId::new(a.identity, physical.allocation() + 1).unwrap();
    let mut held = a.serializable_admission(&control).unwrap();
    let publication = held.graph().publication(committed.id()).unwrap().unwrap();
    held.graph_mut()
        .prepare_publication(unknown.id(), missing, [8; 32], &control)
        .unwrap();
    held.persist(&control).unwrap();
    let held = a.serializable_admission(&control).unwrap();
    let receipt = a.commit(physical, &prepared, &control).unwrap();
    let unknown_id = unknown.id();
    drop((held, a, committed, unknown));
    assert!(matches!(
        b.admit_serializable::<()>(true, &control, || panic!("receipt remains unknown")),
        Err(VersionError::UnknownTransaction)
    ));
    let mut held = b.serializable_admission(&control).unwrap();
    assert_eq!(
        held.graph_mut()
            .resolve_publication(publication, CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert!(matches!(
        held.graph().check_active(unknown_id),
        Err(VersionError::TransactionSealed)
    ));
    held.graph().check_active(survivor.id()).unwrap();
}
