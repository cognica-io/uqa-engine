//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable physical receipts take precedence over lost checkpoint replies and database reopen.

use super::*;

const CHECKPOINT: TableDefinition<u8, &[u8]> = TableDefinition::new("uqa_mvcc_serializable");

#[test]
fn a_sole_participant_keeps_the_exclusive_database_owner_until_its_final_release() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owner.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
    let participant = actor(&store, &control);
    let original = participant.id();
    drop(store);
    assert!(Database::open(&path).is_err());
    drop(participant);
    assert_eq!(control.memory().used(), 0);
    // redb's final Database close may flush its allocator metadata, after the SSI owner and gate have been released.
    let store = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
    let next = actor(&store, &control);
    assert_eq!(next.id().coordinator(), original.coordinator());
    assert!(next.id().allocation() > original.allocation());
    drop(next);
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn reopen_recovers_dead_publishers_without_losing_commit_receipts_or_allocation_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("serializable.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let (old, pending, publication, receipt) = {
        let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
        let pending = actor(&store, &control);
        let committed = actor(&store, &control);
        let (pending_write, _) = prepare(&store, &pending, b"pending", &control);
        let (publication, prepared) = prepare(&store, &committed, b"committed", &control);
        let receipt = graph(&store, &control, |_| {
            Ok(store
                .commit(publication.transaction(), &prepared, &control)
                .unwrap())
        })
        .unwrap();
        // No graph completion is retained, even though the main commit is durable.
        (committed.id(), pending_write, publication, receipt)
    };
    assert_eq!(control.memory().used(), 0);
    let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
    let (next, view) = store.admit_serializable_snapshot(true, &control).unwrap();
    assert_eq!(next.id().coordinator(), old.coordinator());
    assert!(next.id().allocation() > old.allocation());
    assert_eq!(
        store
            .commit_status(pending.transaction(), &control)
            .unwrap(),
        CommitStatus::Aborted
    );
    assert_eq!(
        store
            .commit_status(publication.transaction(), &control)
            .unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert!(view.get(b"pending", &control).unwrap().is_none());
    assert_eq!(
        view.get(b"committed", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|value| &***value),
        Some(&b"durable"[..])
    );
    drop((next, view));
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unknown_receipts_block_capture_while_earlier_confirmed_results_remain_retained() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let survivor = actor(&store, &control);
    let committed = actor(&store, &control);
    let unknown = actor(&store, &control);
    let (publication, prepared) = prepare(&store, &committed, b"committed", &control);
    let missing =
        StorageTransactionId::new(store.identity, publication.transaction().allocation() + 1)
            .unwrap();
    graph(&store, &control, |graph| {
        graph.prepare_publication(unknown.id(), missing, [6; 32], &control)
    })
    .unwrap();
    let receipt = graph(&store, &control, |_| {
        Ok(store
            .commit(publication.transaction(), &prepared, &control)
            .unwrap())
    })
    .unwrap();
    let unknown_id = unknown.id();
    drop((committed, unknown));
    assert!(matches!(
        admit_serializable::<_, ()>(&store, true, &control, || panic!("unknown receipt")),
        Err(VersionError::UnknownTransaction)
    ));
    graph(&store, &control, |graph| {
        assert_eq!(
            graph.resolve_publication(publication, CommitStatus::Unknown)?,
            CommitStatus::Committed(receipt)
        );
        assert!(matches!(
            graph.check_active(unknown_id),
            Err(VersionError::TransactionSealed)
        ));
        graph.check_active(survivor.id())
    })
    .unwrap();
}

#[test]
fn a_failed_checkpoint_commit_cannot_turn_durable_records_into_an_abort() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    let (publication, receipt) = {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database).unwrap();
        let committed = actor(&store, &control);
        let (publication, prepared) = prepare(&store, &committed, b"committed", &control);
        let mut receipt = None;
        assert!(graph(&store, &control, |graph| {
            let durable = store
                .commit(publication.transaction(), &prepared, &control)
                .unwrap();
            receipt = Some(durable);
            graph.resolve_publication(publication, CommitStatus::Committed(durable))?;
            backend.fail_sync.store(true, Ordering::Relaxed);
            Ok(())
        })
        .is_err());
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
        (publication, receipt.unwrap())
    };
    assert_eq!(control.memory().used(), 0);
    let store = RedbRecordStore::new(Arc::new(
        Database::builder().create_with_backend(backend).unwrap(),
    ))
    .unwrap();
    let (next, view) = store.admit_serializable_snapshot(true, &control).unwrap();
    assert_eq!(
        store
            .commit_status(publication.transaction(), &control)
            .unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(
        view.get(b"committed", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|value| &***value),
        Some(&b"durable"[..])
    );
    drop((next, view));
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn malformed_retained_state_is_rejected_before_callbacks_without_reinitialization() {
    for corruption in 0..7 {
        let store = memory();
        let control = StorageReadControl::with_limit(1 << 20);
        let participant = actor(&store, &control);
        let transaction = physical_writer(&store.database).unwrap();
        {
            let mut metadata = transaction.open_table(METADATA).unwrap();
            let mut checkpoint = transaction.open_table(CHECKPOINT).unwrap();
            match corruption {
                0 => {
                    metadata.remove("serializable").unwrap();
                }
                1 => {
                    metadata
                        .insert("serializable", b"unknown".as_slice())
                        .unwrap();
                }
                2 => {
                    checkpoint.remove(0).unwrap();
                }
                3 => {
                    checkpoint.insert(1, b"extra".as_slice()).unwrap();
                }
                4 => {
                    let mut bytes = checkpoint.get(0).unwrap().unwrap().value().to_vec();
                    let end = bytes.len() - 1;
                    bytes[end] ^= 1;
                    checkpoint.insert(0, bytes.as_slice()).unwrap();
                }
                5 => {
                    let mut bytes = metadata
                        .get("serializable")
                        .unwrap()
                        .unwrap()
                        .value()
                        .to_vec();
                    bytes[8] ^= 1;
                    metadata.insert("serializable", bytes.as_slice()).unwrap();
                }
                6 => {}
                _ => unreachable!(),
            }
        }
        if corruption == 6 {
            transaction.delete_table(CHECKPOINT).unwrap();
        }
        transaction.commit().unwrap();
        assert!(
            store
                .with_serializable_admission(&control, &mut |_, _| panic!("corrupt graph admitted"))
                .is_err(),
            "corruption {corruption}"
        );
        // A second independent adapter must still see the original fault, never a fresh coordinator.
        let peer = RedbRecordStore::new(Arc::clone(&store.database)).unwrap();
        assert!(peer.admit_serializable_snapshot(true, &control).is_err());
        drop((participant, store, peer));
        assert_eq!(control.memory().used(), 0);
    }
}
