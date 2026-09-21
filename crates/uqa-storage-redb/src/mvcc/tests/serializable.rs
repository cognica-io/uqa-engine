//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent adapters retain conflicts, exact physical outcomes and original participant ownership.

mod migration;
mod persistence;
mod process;
mod recovery;

use uqa_storage::mvcc::{
    admit_serializable, RecordWrite, SerializableCoordinator, SerializableGraph,
    SerializableKeySpace, SerializableParticipant, SerializablePredicate, SerializablePublication,
};

use super::*;

fn memory() -> RedbRecordStore {
    RedbRecordStore::new(Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    ))
    .unwrap()
}

fn actor(store: &RedbRecordStore, control: &StorageReadControl) -> SerializableParticipant {
    let provider: &dyn VersionedPersistence = store;
    provider
        .serializable_coordinator()
        .unwrap()
        .admit_serializable_snapshot(false, control)
        .unwrap()
        .0
}

fn graph<T>(
    store: &RedbRecordStore,
    control: &StorageReadControl,
    operation: impl FnOnce(&mut SerializableGraph) -> VersionResult<T>,
) -> VersionResult<T> {
    let mut operation = Some(operation);
    let mut result = None;
    store.with_serializable_admission(control, &mut |graph, _| {
        result = Some(operation.take().expect("single invocation")(graph)?);
        Ok(())
    })?;
    Ok(result.expect("admission invoked its operation"))
}

fn prepare(
    store: &RedbRecordStore,
    actor: &SerializableParticipant,
    key: &[u8],
    control: &StorageReadControl,
) -> (SerializablePublication, PreparedRecordCommit) {
    let physical = store.allocate_transaction(control).unwrap();
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key,
            expected: None,
            value: Some(b"durable"),
        }],
        control,
    )
    .unwrap();
    let publication = graph(store, control, |graph| {
        graph.prepare_publication(actor.id(), physical, prepared.fingerprint(), control)
    })
    .unwrap();
    (publication, prepared)
}

#[test]
fn adapters_keep_original_participants_and_database_ownership_after_handle_churn() {
    let first = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let database = Arc::downgrade(&first.database);
    let state = Arc::downgrade(&first.serializable);
    let (participant, view) = first.admit_serializable_snapshot(false, &control).unwrap();
    let id = participant.id();
    let nested = participant.clone();
    drop((first, participant, view));
    let next = RedbRecordStore::new(database.upgrade().unwrap()).unwrap();
    assert!(Arc::ptr_eq(&state.upgrade().unwrap(), &next.serializable));
    next.recover_serializable_participants(&control).unwrap();
    graph(&next, &control, |graph| graph.check_active(id)).unwrap();
    next.with_serializable_admission(&control, &mut |graph, leases| {
        graph.check_active(id)?;
        drop(nested.clone());
        assert!(leases.is_alive(id, &control)?);
        Ok(())
    })
    .unwrap();
    drop(nested);
    next.recover_serializable_participants(&control).unwrap();
    graph(&next, &control, |graph| {
        assert!(graph.check_active(id).is_err());
        let new = graph.admit(true, &control)?;
        assert!(new.allocation() > id.allocation());
        assert_eq!(new.coordinator(), id.coordinator());
        graph.rollback(new)?;
        graph.reclaim();
        Ok(())
    })
    .unwrap();
    assert_eq!(control.memory().used(), 0);
    drop(next);
    assert!(state.upgrade().is_none());
    assert!(database.upgrade().is_none());
}

#[test]
fn retained_predicates_reject_write_skew_across_independent_record_adapters() {
    let first = memory();
    let second = RedbRecordStore::new(Arc::clone(&first.database)).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let a = actor(&first, &control);
    let b = actor(&second, &control);
    for (store, actor) in [(&first, &a), (&second, &b)] {
        graph(store, &control, |graph| {
            graph.observe_read(actor.id(), SerializablePredicate::object([5; 16]), &control)
        })
        .unwrap();
    }
    for (store, actor, key) in [(&first, &a, b"a"), (&second, &b, b"b")] {
        graph(store, &control, |graph| {
            graph.observe_write(
                actor.id(),
                SerializablePredicate::point([5; 16], SerializableKeySpace::Rows, key),
                &control,
            )
        })
        .unwrap();
    }
    let (publication, prepared) = prepare(&first, &a, b"a", &control);
    graph(&first, &control, |graph| {
        let receipt = first
            .commit(publication.transaction(), &prepared, &control)
            .unwrap();
        graph.resolve_publication(publication, CommitStatus::Committed(receipt))?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        graph(&second, &control, |graph| graph.prepare_commit(b.id(), &control)),
        Err(VersionError::SerializationConflict { transaction }) if transaction == b.id()
    ));
    drop((a, b, prepared));
    second.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_capture_consumes_admission_and_cancellation_does_not_replay_it() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let survivor = actor(&store, &control);
    let mut calls = 0;
    for cancel in [false, true] {
        let result: VersionResult<(SerializableParticipant, ())> =
            admit_serializable(&store, false, &control, || {
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
    let next = actor(&store, &control);
    assert_eq!(next.id().allocation(), survivor.id().allocation() + 3);
    graph(&store, &control, |graph| graph.check_active(survivor.id())).unwrap();
    drop((survivor, next));
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn decode_and_encoding_exhaustion_preserve_the_retained_checkpoint() {
    let store = memory();
    let zero = StorageReadControl::with_limit(0);
    assert!(store.admit_serializable_snapshot(false, &zero).is_err());
    assert_eq!(zero.memory().used(), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let survivor = actor(&store, &control);
    let tiny = StorageReadControl::with_limit(1);
    assert!(store
        .with_serializable_admission(&tiny, &mut |_, _| panic!("decode exceeded its allowance"))
        .is_err());
    assert_eq!(tiny.memory().used(), 0);
    let mut exhausted = None;
    let result = graph(&store, &control, |graph| {
        graph.rollback(survivor.id())?;
        exhausted = Some(
            control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())?,
        );
        Ok(())
    });
    assert!(matches!(result, Err(VersionError::Memory(_))));
    drop(exhausted);
    graph(&store, &control, |graph| graph.check_active(survivor.id())).unwrap();
    drop(survivor);
    store.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn concurrent_admission_over_independent_adapters_has_one_allocation_order() {
    use std::sync::Barrier;

    let first = memory();
    let second = RedbRecordStore::new(Arc::clone(&first.database)).unwrap();
    let barrier = Barrier::new(2);
    let control = StorageReadControl::with_limit(1 << 20);
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            actor(&first, &control)
        });
        barrier.wait();
        let b = actor(&second, &control);
        (a.join().unwrap(), b)
    });
    assert_eq!(a.id().coordinator(), b.id().coordinator());
    assert_eq!(a.id().allocation().abs_diff(b.id().allocation()), 1);
    graph(&first, &control, |graph| {
        graph.check_active(a.id())?;
        graph.check_active(b.id())
    })
    .unwrap();
    drop((a, b));
    first.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn read_only_completion_remains_resolvable_while_its_original_handle_is_retained() {
    use uqa_storage::mvcc::SerializableStatus;

    let store = memory();
    let peer = RedbRecordStore::new(Arc::clone(&store.database)).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let (completed, view) = store.admit_serializable_snapshot(true, &control).unwrap();
    graph(&store, &control, |graph| {
        graph.prepare_commit(completed.id(), &control)?;
        graph.commit(completed.id())
    })
    .unwrap();
    let next = actor(&peer, &control);
    graph(&peer, &control, |graph| {
        assert_eq!(graph.status(completed.id())?, SerializableStatus::Committed);
        assert!(graph.publication(completed.id())?.is_none());
        graph.commit(completed.id())
    })
    .unwrap();
    assert_eq!(peer.allocate_transaction(&control).unwrap().allocation(), 1);
    drop((completed, view, next));
    peer.recover_serializable_participants(&control).unwrap();
    assert_eq!(control.memory().used(), 0);
}
