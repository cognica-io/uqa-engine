//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier allocation belongs to the durable owner and is never undone with private rows.

use super::*;

#[test]
fn identifier_batches_preserve_durable_observations_across_private_undo() {
    let persistence = Persistence::new();
    verify_identifier_batches(&persistence.session(1 << 20), &persistence.session(1 << 20))
        .unwrap();
}

#[test]
fn failed_or_unwound_batch_evaluation_cannot_observe_an_identifier() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.begin_transaction().unwrap();
    store.put(b"prior", b"kept").unwrap();
    assert!(store
        .with_mutation(&mut |_, batch| {
            batch.observe_identifier(b"observed", 999)?;
            batch.put(b"discarded", b"row")?;
            Err(StorageBackendError::Other("rejected evaluation".into()))
        })
        .is_err());
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.with_mutation(&mut |_, batch| {
            batch.observe_identifier(b"observed", 999)?;
            panic!("unwound evaluation");
        })
    }));
    assert!(unwind.is_err());
    assert!(persistence.state.lock().identifiers.is_empty());
    assert_eq!(store.get(b"prior").unwrap().as_deref(), Some(&b"kept"[..]));
    assert!(store.get(b"discarded").unwrap().is_none());
    store.rollback_transaction().unwrap();
}

#[test]
fn failed_batch_observation_restores_its_rows_and_preserves_earlier_private_writes() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.begin_transaction().unwrap();
    store.put(b"prior", b"kept").unwrap();
    persistence.state.lock().identifier_fault = true;
    let error = store
        .with_mutation(&mut |_, batch| {
            batch.observe_identifier(b"observed", 100)?;
            batch.put(b"discarded", b"row")
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("injected identifier failure"),
        "{error}"
    );
    assert_eq!(store.get(b"prior").unwrap().as_deref(), Some(&b"kept"[..]));
    assert!(store.get(b"discarded").unwrap().is_none());
    assert!(persistence.state.lock().identifiers.is_empty());
    assert_eq!(persistence.state.lock().next, 0);
    persistence.state.lock().identifier_fault = false;
    store.rollback_transaction().unwrap();
    assert_eq!(
        store
            .allocate_identifiers(b"observed", one())
            .unwrap()
            .watermark(),
        1
    );
}

#[test]
fn batch_observations_finish_before_record_publication_and_are_not_replayed_on_retry() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let other = persistence.session(1 << 20);
    persistence.state.lock().commit_fault = CommitFault::Reject;
    let mut batch = store.batch();
    batch.observe_identifier(b"observed", 100).unwrap();
    batch.put(b"row", b"pending").unwrap();
    assert!(batch.commit().is_err());
    assert!(other.get(b"row").unwrap().is_none());
    assert_eq!(
        other
            .allocate_identifiers(b"observed", one())
            .unwrap()
            .watermark(),
        101
    );
    let mut sealed = store.batch();
    sealed.observe_identifier(b"observed", 999).unwrap();
    assert!(sealed.commit().unwrap_err().to_string().contains("sealed"));
    assert_eq!(store.identifier_watermark(b"observed").unwrap(), Some(101));
    assert!(store.in_transaction());
    assert!(other.get(b"row").unwrap().is_none());
    // A sealed record retry must not resubmit the already durable observation.
    {
        let mut state = persistence.state.lock();
        state.commit_fault = CommitFault::None;
        state.identifier_fault = true;
    }
    store.commit_transaction().unwrap();
    assert_eq!(other.get(b"row").unwrap().as_deref(), Some(&b"pending"[..]));
    assert_eq!(
        persistence.state.lock().identifiers[b"observed".as_slice()],
        101
    );
}

#[test]
fn batch_observations_allocate_once_for_each_run_of_one_namespace() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let mut batch = store.batch();
    for value in [5, 9, 7] {
        batch.observe_identifier(b"rows", value).unwrap();
    }
    batch.observe_identifier(b"other", 3).unwrap();
    batch.observe_identifier(b"rows", 4).unwrap();
    batch.inherit_identifiers(b"rows", b"successor").unwrap();
    batch.observe_identifier(b"successor", 2).unwrap();
    batch.observe_identifier(b"successor", 20).unwrap();
    batch.put(b"row", b"value").unwrap();
    batch.commit().unwrap();
    let state = persistence.state.lock();
    let requests = state
        .identifier_requests
        .iter()
        .map(|(namespace, value)| (namespace.as_slice(), *value))
        .collect::<Vec<_>>();
    // The second run of `rows` observes a value its first run already covers. An inheritance reads its source through an allocation.
    assert_eq!(
        requests,
        [
            (&b"rows"[..], Some(9)),
            (&b"other"[..], Some(3)),
            (&b"rows"[..], Some(0)),
            (&b"successor"[..], Some(9)),
            (&b"successor"[..], Some(20)),
        ]
    );
    assert_eq!(state.identifiers[b"rows".as_slice()], 9);
    assert_eq!(state.identifiers[b"other".as_slice()], 3);
    assert_eq!(state.identifiers[b"successor".as_slice()], 20);
}

#[test]
fn an_observation_its_session_already_covers_allocates_nothing() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let other = persistence.session(1 << 20);
    let observe = |store: &VersionedKeyValueStore, value: u64| {
        let mut batch = store.batch();
        batch.observe_identifier(b"rows", value).unwrap();
        batch.put(b"row", &value.to_be_bytes()).unwrap();
        batch.commit().unwrap();
    };
    let requests = || {
        persistence
            .state
            .lock()
            .identifier_requests
            .iter()
            .map(|(_, value)| *value)
            .collect::<Vec<_>>()
    };
    // A row rewrite observes the identity its row already has, and a lower identity is covered as well.
    observe(&store, 10);
    observe(&store, 10);
    observe(&store, 3);
    assert_eq!(requests(), [Some(10)]);

    // What a session knows is a lower bound: a larger value allocates, and reads back what another session reserved.
    assert_eq!(
        other
            .allocate_identifiers(b"rows", one())
            .unwrap()
            .watermark(),
        11
    );
    observe(&store, 11);
    observe(&store, 11);
    assert_eq!(requests(), [Some(10), None, Some(11)]);

    // The reserving session covers what it reserved. A third session knows nothing, and its observation never lowers the watermark.
    observe(&other, 5);
    assert_eq!(requests(), [Some(10), None, Some(11)]);
    observe(&persistence.session(1 << 20), 5);
    assert_eq!(requests(), [Some(10), None, Some(11), Some(5)]);
    assert_eq!(persistence.state.lock().identifiers[b"rows".as_slice()], 11);

    // A rolled back transaction keeps its observation, so the session still covers it.
    store.begin_transaction().unwrap();
    store
        .with_mutation(&mut |_, batch| batch.observe_identifier(b"rows", 12))
        .unwrap();
    store.rollback_transaction().unwrap();
    observe(&store, 12);
    assert_eq!(requests(), [Some(10), None, Some(11), Some(5), Some(12)]);
    assert_eq!(persistence.state.lock().identifiers[b"rows".as_slice()], 12);

    // A reservation of this session covers the identities it returned.
    let reserved = store
        .allocate_identifiers(b"rows", one())
        .unwrap()
        .watermark();
    assert_eq!(reserved, 13);
    observe(&store, reserved);
    assert_eq!(
        requests(),
        [Some(10), None, Some(11), Some(5), Some(12), None]
    );

    // A failed observation is not remembered.
    persistence.state.lock().identifier_fault = true;
    let mut failed = store.batch();
    failed.observe_identifier(b"rows", 14).unwrap();
    assert!(failed.commit().is_err());
    persistence.state.lock().identifier_fault = false;
    observe(&store, 14);
    assert_eq!(persistence.state.lock().identifiers[b"rows".as_slice()], 14);
}

#[test]
fn an_observation_ahead_of_its_rows_covers_them_under_the_admission_of_an_allocation() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let requests = || {
        persistence
            .state
            .lock()
            .identifier_requests
            .iter()
            .map(|(_, value)| *value)
            .collect::<Vec<_>>()
    };
    // A statement raises the watermark to the greatest identity it supplies, and the rows it then writes observe identities that covers.
    store.observe_identifier(b"rows", 50).unwrap();
    let mut batch = store.batch();
    for value in [1, 25, 50] {
        batch.observe_identifier(b"rows", value).unwrap();
    }
    batch.put(b"row", b"value").unwrap();
    batch.commit().unwrap();
    assert_eq!(requests(), [Some(50)]);

    // A covered identity is answered from what the session has read; an allocation reports the current watermark and stays physical.
    store.observe_identifier(b"rows", 20).unwrap();
    assert_eq!(requests(), [Some(50)]);
    assert_eq!(
        store
            .allocate_identifiers(b"rows", IdentifierRequest::Observe(20))
            .unwrap()
            .watermark(),
        50
    );
    store.observe_identifier(b"rows", 51).unwrap();
    assert_eq!(requests(), [Some(50), Some(20), Some(51)]);
    assert_eq!(persistence.state.lock().identifiers[b"rows".as_slice()], 51);

    // A covered identity is refused wherever an allocation is refused.
    store.begin_read_transaction().unwrap();
    let error = store.observe_identifier(b"rows", 20).unwrap_err();
    assert!(error.to_string().contains("read-only"), "{error}");
    store.commit_transaction().unwrap();
    assert_eq!(requests(), [Some(50), Some(20), Some(51)]);

    // A failed observation is not remembered.
    persistence.state.lock().identifier_fault = true;
    assert!(store.observe_identifier(b"rows", 60).is_err());
    persistence.state.lock().identifier_fault = false;
    store.observe_identifier(b"rows", 60).unwrap();
    assert_eq!(persistence.state.lock().identifiers[b"rows".as_slice()], 60);
}

#[test]
fn rejected_record_staging_does_not_consume_queued_identifiers() {
    let persistence = Persistence::new();
    let mut failures = 0;
    let mut successes = 0;
    for headroom in (0_usize..=8192).step_by(64) {
        let store = persistence.session(1 << 20);
        store.begin_transaction().unwrap();
        store.put(b"prior", b"kept").unwrap();
        let mut batch = store.batch();
        let namespace = headroom.to_be_bytes();
        batch.observe_identifier(&namespace, 100).unwrap();
        batch.put(b"small", b"row").unwrap();
        batch.put(b"large", &[7; 4096]).unwrap();
        let control = store.retention_control();
        let hold = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used() - headroom)
            .unwrap();
        let outcome = batch.commit();
        drop(hold);
        match outcome {
            Ok(()) => {
                successes += 1;
                assert_eq!(
                    persistence.state.lock().identifiers[namespace.as_slice()],
                    100
                );
            }
            Err(StorageBackendError::Memory(_)) => {
                failures += 1;
                assert!(!persistence
                    .state
                    .lock()
                    .identifiers
                    .contains_key(namespace.as_slice()));
                assert!(store.get(b"small").unwrap().is_none());
                assert!(store.get(b"large").unwrap().is_none());
            }
            Err(error) => panic!("unexpected staging failure: {error}"),
        }
        assert_eq!(store.get(b"prior").unwrap().as_deref(), Some(&b"kept"[..]));
        store.rollback_transaction().unwrap();
    }
    assert!(failures > 0 && successes > 0);
}

#[test]
fn document_allocations_use_the_backend_capability_across_private_undo() {
    use uqa_storage::{KeyValueCatalog, KeyValueStorageBackend, PersistentStorageSession};
    let persistence = Persistence::new();
    let pair = || {
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
        PersistentStorageSession::new(
            Arc::new(KeyValueCatalog::new(store.clone())),
            Arc::new(KeyValueStorageBackend::new(store)),
        )
    };
    uqa_storage::document_store::identifiers::conformance::verify_document_id_sessions(
        &pair(),
        &pair(),
    )
    .unwrap();
}

#[test]
fn document_identifier_migration_retires_legacy_metadata_only_with_its_catalog_commit() {
    use uqa_storage::document_store::identifiers::{
        legacy_document_id_metadata_key, DocumentIdAllocator,
    };
    use uqa_storage::{CatalogFacade, KeyValueCatalog};
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 20));
    let other = Arc::new(persistence.session(1 << 20));
    let catalog = KeyValueCatalog::new(store.clone());
    let observer = KeyValueCatalog::new(other.clone());
    let key = legacy_document_id_metadata_key("public.docs");
    catalog.set_metadata(&key, "500").unwrap();
    let allocator =
        DocumentIdAllocator::new(store.identifier_allocator(), [1; 16], [2; 16]).unwrap();
    store.begin_transaction().unwrap();
    allocator
        .persist(&catalog, "public.docs", &mut 500)
        .unwrap();
    assert_eq!(catalog.get_metadata(&key).unwrap().as_deref(), Some(""));
    assert_eq!(observer.get_metadata(&key).unwrap().as_deref(), Some("500"));
    persistence.state.lock().commit_fault = CommitFault::Reject;
    assert!(store.commit_transaction().is_err());
    store.rollback_transaction().unwrap();
    assert_eq!(catalog.get_metadata(&key).unwrap().as_deref(), Some("500"));
    let independent =
        DocumentIdAllocator::new(other.identifier_allocator(), [1; 16], [2; 16]).unwrap();
    assert_eq!(independent.allocate(&mut 1).unwrap(), 500);
    persistence.state.lock().commit_fault = CommitFault::None;
    store.begin_transaction().unwrap();
    let mut next = 500;
    allocator
        .persist(&catalog, "public.docs", &mut next)
        .unwrap();
    assert_eq!(next, 501);
    store.commit_transaction().unwrap();
    assert_eq!(observer.get_metadata(&key).unwrap().as_deref(), Some(""));
}

#[test]
fn document_allocator_capability_preserves_read_only_errors_and_the_local_cache() {
    use uqa_storage::document_store::identifiers::DocumentIdAllocator;
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let allocator =
        DocumentIdAllocator::new(store.identifier_allocator(), [1; 16], [2; 16]).unwrap();
    store.begin_read_transaction().unwrap();
    let mut next = 1;
    assert!(allocator.allocate(&mut next).is_err());
    assert_eq!(next, 1);
    // A supplied identity moves only the local floor; the row that supplies it makes it durable when published.
    let mut floor = next;
    DocumentIdAllocator::observe(&mut floor, 100).unwrap();
    assert_eq!(floor, 101);
    store.rollback_transaction().unwrap();
    assert_eq!(allocator.allocate(&mut next).unwrap(), 1);
}

fn one() -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum: 1,
        maximum: u64::MAX,
        count: std::num::NonZeroU64::new(1).unwrap(),
    }
}

#[test]
fn durable_identifier_reservations_share_the_physical_conformance_contract() {
    let persistence = Persistence::new();
    verify_identifier_allocations(
        &*persistence,
        &*persistence,
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap();
}

#[test]
fn identifier_allocations_do_not_publish_private_records_and_survive_undo() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.begin_transaction().unwrap();
    a.put(b"private", b"row").unwrap();
    a.savepoint("before-reservation").unwrap();
    assert_eq!(
        a.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(1..=1)
    );
    assert_eq!(
        b.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(2..=2)
    );
    assert!(b.get(b"private").unwrap().is_none());
    assert_eq!(persistence.state.lock().next, 0);
    a.rollback_to_savepoint("before-reservation").unwrap();
    assert_eq!(
        a.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(3..=3)
    );
    a.rollback_transaction().unwrap();
    assert_eq!(
        a.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(4..=4)
    );
    assert!(!a.in_transaction());
    assert!(a.get(b"private").unwrap().is_none());
}

#[test]
fn identifier_allocations_reject_read_only_sessions_and_sealed_attempts() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.begin_read_transaction().unwrap();
    assert!(a.allocate_identifiers(b"entities", one()).is_err());
    a.rollback_transaction().unwrap();
    a.begin_transaction().unwrap();
    a.put(b"row", b"private").unwrap();
    persistence.state.lock().commit_fault = CommitFault::Reject;
    assert!(a.commit_transaction().is_err());
    let error = a.allocate_identifiers(b"entities", one()).unwrap_err();
    assert!(error.to_string().contains("sealed"), "{error}");
    assert_eq!(
        b.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(1..=1)
    );
    persistence.state.lock().commit_fault = CommitFault::None;
    a.commit_transaction().unwrap();
}
