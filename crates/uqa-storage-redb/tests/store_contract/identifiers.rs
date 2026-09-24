//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable identifiers share redb's database owner while remaining independent of logical record undo.

use std::num::NonZeroU64;
use uqa_storage::mvcc::{verify_identifier_allocations, IdentifierRequest, VersionedPersistence};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::KeyValueStore;
use uqa_storage_redb::RedbStorage;

#[test]
fn identifier_batches_survive_private_undo_and_closed_file_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identifier-batches.redb");
    let last = {
        let storage = RedbStorage::open(&path).unwrap();
        uqa_storage::mvcc::verify_identifier_batches(&storage.store(), &storage.store()).unwrap()
    };
    let reopened = RedbStorage::open(&path).unwrap();
    assert_eq!(
        reopened
            .store()
            .identifier_allocator()
            .unwrap()
            .identifier_watermark(b"identifier-batches")
            .unwrap(),
        Some(last)
    );
    assert_eq!(
        reopened
            .store()
            .identifier_allocator()
            .unwrap()
            .allocate_identifiers(b"identifier-batches", request())
            .unwrap()
            .watermark(),
        last + 1
    );
}

#[test]
fn document_id_backends_reserve_independently_and_reopen() {
    use uqa_storage::document_store::identifiers::{
        conformance::verify_document_id_sessions, DocumentIdAllocator,
    };
    use uqa_storage::PersistentStorageProvider;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("document-identifiers.redb");
    let last = {
        let storage = RedbStorage::open(&path).unwrap();
        verify_document_id_sessions(
            &storage.open_session().unwrap(),
            &storage.open_session().unwrap(),
        )
        .unwrap()
    };
    let reopened = RedbStorage::open(&path).unwrap();
    let session = reopened.open_session().unwrap();
    let ids = DocumentIdAllocator::new(session.backend.identifier_allocator(), [11; 16], [12; 16])
        .unwrap();
    assert_eq!(ids.allocate(&mut 1).unwrap(), last + 1);
}

fn request() -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum: 1,
        maximum: u64::MAX,
        count: NonZeroU64::new(1).unwrap(),
    }
}

#[test]
fn identifier_reservations_survive_private_undo_and_closed_file_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identifiers.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let identity = {
        let storage = RedbStorage::open(&path).unwrap();
        let a = storage.record_store().unwrap();
        let b = storage.record_store().unwrap();
        verify_identifier_allocations(&a, &b, &control).unwrap();
        let session = storage.store();
        let observer = storage.store();
        session.begin_transaction().unwrap();
        session.put(b"private", b"uncommitted").unwrap();
        session.savepoint("before-allocation").unwrap();
        assert_eq!(
            a.allocate_identifiers(b"entities", request(), &control)
                .unwrap()
                .range(),
            Some(1..=1)
        );
        assert_eq!(
            b.allocate_identifiers(b"entities", request(), &control)
                .unwrap()
                .range(),
            Some(2..=2)
        );
        assert!(observer.get(b"private").unwrap().is_none());
        session.rollback_to_savepoint("before-allocation").unwrap();
        session.rollback_transaction().unwrap();
        assert_eq!(
            a.allocate_identifiers(b"entities", request(), &control)
                .unwrap()
                .range(),
            Some(3..=3)
        );
        a.database_id()
    };
    let reopened = RedbStorage::open(&path).unwrap();
    let records = reopened.record_store().unwrap();
    assert_eq!(records.database_id(), identity);
    assert_eq!(
        records.identifier_watermark(b"entities", &control).unwrap(),
        Some(3)
    );
    assert!(reopened.store().get(b"private").unwrap().is_none());
    assert_eq!(
        records
            .allocate_identifiers(b"entities", request(), &control)
            .unwrap()
            .range(),
        Some(4..=4)
    );
}
