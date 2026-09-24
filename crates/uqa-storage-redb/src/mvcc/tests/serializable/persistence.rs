//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! State inquiries and repeated predicates do not acquire a physical writer or publish pages.

use super::*;

fn counts(backend: &FaultBackend) -> (usize, usize) {
    (
        backend.writes.load(Ordering::Relaxed),
        backend.syncs.load(Ordering::Relaxed),
    )
}

#[test]
fn empty_initialization_is_retained_but_repeated_observations_do_not_publish() {
    let backend = FaultBackend::default();
    let store = RedbRecordStore::new(Arc::new(
        Database::builder()
            .create_with_backend(backend.clone())
            .unwrap(),
    ))
    .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let coordinator = graph(&store, &control, |graph| Ok(graph.coordinator())).unwrap();
    let before = counts(&backend);
    graph(&store, &control, |graph| {
        assert_eq!(graph.coordinator(), coordinator);
        assert!(!graph.checkpoint_changed());
        Ok(())
    })
    .unwrap();
    assert_eq!(counts(&backend), before);
    let participant = actor(&store, &control);
    let predicate = SerializablePredicate::object([5; 16]);
    graph(&store, &control, |graph| {
        graph.observe_read(participant.id(), predicate, &control)
    })
    .unwrap();
    let before = counts(&backend);
    for _ in 0..3 {
        store.recover_serializable_participants(&control).unwrap();
        graph(&store, &control, |graph| {
            graph.check_active(participant.id())?;
            graph.observe_read(participant.id(), predicate, &control)
        })
        .unwrap();
    }
    assert_eq!(counts(&backend), before);
    // A failed operation also leaves the durable pages untouched when it changed nothing.
    assert!(matches!(
        graph(&store, &control, |_| Err::<(), _>(
            VersionError::TransactionSealed
        )),
        Err(VersionError::TransactionSealed)
    ));
    assert_eq!(counts(&backend), before);
    graph(&store, &control, |graph| {
        graph.observe_read(
            participant.id(),
            SerializablePredicate::object([6; 16]),
            &control,
        )
    })
    .unwrap();
    let after = counts(&backend);
    assert!(after.0 > before.0 && after.1 > before.1);
}

#[test]
fn unchanged_admission_completes_while_an_independent_main_writer_is_held() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let participant = actor(&store, &control);
    let writer = physical_writer(&store.database).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            send.send(graph(&store, &control, |graph| {
                graph.check_active(participant.id())
            }))
            .unwrap();
        });
        let completed = receive.recv_timeout(std::time::Duration::from_secs(10));
        // Release before checking the result so a regression cannot strand the test worker.
        drop(writer);
        completed
            .expect("unchanged SSI admission waited for the main writer")
            .unwrap();
    });
}
