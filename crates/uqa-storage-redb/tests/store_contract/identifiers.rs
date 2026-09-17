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
    assert!(reopened.store().get(b"private").unwrap().is_none());
    assert_eq!(
        records
            .allocate_identifiers(b"entities", request(), &control)
            .unwrap()
            .range(),
        Some(4..=4)
    );
}
