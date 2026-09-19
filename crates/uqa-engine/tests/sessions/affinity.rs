//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine attachment rejects catalog/data pairs from different transaction contexts.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use uqa_engine::Engine;
use uqa_storage::{
    PersistentStorageProvider, PersistentStorageSession, StorageBackendError, StorageBackendResult,
    StorageSessionMismatch,
};
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

fn expect_mismatch(result: StorageBackendResult<Engine>) {
    match result {
        Err(StorageBackendError::Backend { source, .. }) => {
            assert!(
                source.downcast_ref::<StorageSessionMismatch>().is_some(),
                "{source}"
            );
        }
        Err(error) => panic!("unexpected attachment error: {error}"),
        Ok(_) => panic!("different transaction contexts were accepted"),
    }
}

#[test]
fn manually_paired_handles_are_checked_for_all_persistent_layouts() {
    let directory = tempfile::tempdir().unwrap();
    let providers: Vec<Arc<dyn PersistentStorageProvider>> = vec![
        Arc::new(SQLiteStorageProvider::new(
            ManagedConnection::open(&directory.path().join("native.db")).unwrap(),
        )),
        Arc::new(SQLiteKeyValueStorage::open(&directory.path().join("kv.db")).unwrap()),
        Arc::new(RedbStorage::open(directory.path().join("redb.db")).unwrap()),
    ];
    for provider in providers {
        let a = provider.open_session().unwrap();
        let b = provider.open_session().unwrap();
        let before = a.backend.change_version().unwrap();
        expect_mismatch(Engine::from_persistent_backends(
            a.catalog.clone(),
            b.backend.clone(),
        ));
        assert!(!a.backend.in_transaction());
        assert!(!b.backend.in_transaction());
        assert_eq!(a.backend.change_version().unwrap(), before);

        let engine = Engine::from_persistent_backends(a.catalog, a.backend).unwrap();
        engine.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
        engine
            .sql("BEGIN; INSERT INTO items VALUES (1); ROLLBACK", &[])
            .unwrap();
        assert!(engine
            .sql("SELECT * FROM items", &[])
            .unwrap()
            .rows
            .is_empty());
        let sibling = engine.new_session().unwrap();
        assert!(sibling
            .sql("SELECT * FROM items", &[])
            .unwrap()
            .rows
            .is_empty());
    }
}

struct MispairedProvider {
    storage: SQLiteKeyValueStorage,
    mismatch: AtomicBool,
}

impl PersistentStorageProvider for MispairedProvider {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let mut session = self.storage.open_session()?;
        if self.mismatch.load(Ordering::Acquire) {
            session.backend = self.storage.open_session()?.backend;
        }
        Ok(session)
    }
}

#[test]
fn provider_pairs_are_checked_during_initial_restore_and_shared_catalog_attachment() {
    let provider = Arc::new(MispairedProvider {
        storage: SQLiteKeyValueStorage::open_in_memory().unwrap(),
        mismatch: AtomicBool::new(true),
    });
    let store = provider.storage.store();
    expect_mismatch(Engine::from_persistent_provider(provider.clone()));
    assert!(uqa_storage::KeyValueStore::scan_prefix(&*store, b"")
        .unwrap()
        .is_empty());

    provider.mismatch.store(false, Ordering::Release);
    let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
    engine.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
    engine.sql("INSERT INTO items VALUES (7)", &[]).unwrap();
    provider.mismatch.store(true, Ordering::Release);
    expect_mismatch(engine.new_session());
    assert_eq!(
        engine.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        1
    );

    provider.mismatch.store(false, Ordering::Release);
    let sibling = engine.new_session().unwrap();
    assert_eq!(
        sibling.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        1
    );
}
