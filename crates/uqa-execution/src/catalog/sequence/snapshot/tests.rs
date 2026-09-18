//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_storage::{KeyValueCatalog, KeyValueStorageBackend, KeyValueStore, MemoryKeyValueStore};

fn session() -> PersistentStorageSession {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    PersistentStorageSession::new(
        Arc::new(KeyValueCatalog::new(Arc::clone(&store))),
        Arc::new(KeyValueStorageBackend::new(store)),
    )
}

#[test]
fn sequence_snapshot_read_releases_its_transaction_after_success_error_and_unwind() {
    let session = session();
    session.catalog.set_metadata("stable", "before").unwrap();
    let result = with_read_transaction(&session, |catalog| {
        assert!(session.backend.in_transaction());
        catalog.get_metadata("stable")
    })
    .unwrap();
    assert_eq!(result.as_deref(), Some("before"));
    assert!(!session.backend.in_transaction());
    let error = with_read_transaction(&session, |_| {
        Err::<(), _>(StorageBackendError::Other(
            "invalid sequence metadata".into(),
        ))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "invalid sequence metadata");
    assert!(!session.backend.in_transaction());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_read_transaction(&session, |_| -> StorageBackendResult<()> {
            panic!("interrupted catalog decoding");
        })
    }));
    assert!(panic.is_err());
    assert!(!session.backend.in_transaction());
    assert_eq!(
        session.catalog.get_metadata("stable").unwrap().as_deref(),
        Some("before")
    );
}

#[test]
fn sequence_snapshot_read_never_adopts_or_finishes_a_callers_transaction() {
    let session = session();
    session.backend.begin_transaction().unwrap();
    session.catalog.set_metadata("private", "pending").unwrap();
    let entered = Cell::new(false);
    let error = with_read_transaction(&session, |_| {
        entered.set(true);
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("idle independent session"));
    assert!(!entered.get());
    assert!(session.backend.in_transaction());
    assert_eq!(
        session.catalog.get_metadata("private").unwrap().as_deref(),
        Some("pending")
    );
    session.backend.rollback_transaction().unwrap();
    assert_eq!(session.catalog.get_metadata("private").unwrap(), None);
}
