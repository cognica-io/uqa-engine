//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier allocation belongs to the durable owner and is never undone with private rows.

use super::*;

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
    assert!(allocator.observe(&mut next, 100).is_err());
    assert_eq!(next, 1);
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
