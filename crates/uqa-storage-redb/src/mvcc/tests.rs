//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native storage failures and fail-closed format metadata.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use redb::{backends::InMemoryBackend, StorageBackend};

use super::*;

mod identifiers;
mod migration;

#[derive(Clone, Debug, Default)]
struct FaultBackend {
    storage: Arc<InMemoryBackend>,
    fail_sync: Arc<AtomicBool>,
}

impl StorageBackend for FaultBackend {
    fn len(&self) -> io::Result<u64> {
        self.storage.len()
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        self.storage.read(offset, out)
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        self.storage.set_len(len)
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.storage.write(offset, bytes)
    }
    fn sync_data(&self) -> io::Result<()> {
        if self.fail_sync.swap(false, Ordering::Relaxed) {
            Err(io::Error::other("injected synchronization failure"))
        } else {
            self.storage.sync_data()
        }
    }
}

#[test]
fn failed_native_commit_reports_uncertainty_and_recovery_resolves_atomic_records() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    let prepared = PreparedRecordCommit::new(
        &[
            uqa_storage::mvcc::RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"one"),
            },
            uqa_storage::mvcc::RecordWrite {
                key: b"b",
                expected: None,
                value: Some(b"two"),
            },
        ],
        &control,
    )
    .unwrap();
    let id = {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database).unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(
            matches!(store.commit(id, &prepared, &control), Err(CommitFailure::Indeterminate { transaction, .. }) if transaction == id)
        );
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
        id
    };
    let database = Arc::new(Database::builder().create_with_backend(backend).unwrap());
    let store = RedbRecordStore::new(database).unwrap();
    let status = store.commit_status(id, &control).unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    match status {
        CommitStatus::Pending => {
            assert!(snapshot.get(b"a", &control).unwrap().is_none());
            assert!(snapshot.get(b"b", &control).unwrap().is_none());
        }
        CommitStatus::Committed(receipt) => {
            assert_eq!(receipt.fingerprint, prepared.fingerprint());
            for (key, expected) in [(b"a", b"one"), (b"b", b"two")] {
                assert_eq!(
                    &***snapshot
                        .get(key, &control)
                        .unwrap()
                        .unwrap()
                        .value()
                        .unwrap(),
                    expected
                );
            }
        }
        other => panic!("recovery lost its allocated transaction: {other:?}"),
    }
    let receipt = store.commit(id, &prepared, &control).unwrap();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
}

#[test]
fn unknown_format_is_rejected_without_reinitializing_existing_metadata() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    RedbRecordStore::new(Arc::clone(&database)).unwrap();
    let transaction = database.begin_write().unwrap();
    transaction
        .open_table(METADATA)
        .unwrap()
        .insert("format", 99_u64.to_be_bytes().as_slice())
        .unwrap();
    transaction.commit().unwrap();
    assert!(matches!(
        RedbRecordStore::new(Arc::clone(&database)),
        Err(VersionError::InvalidEncoding(_))
    ));
    let transaction = database.begin_read().unwrap();
    assert_eq!(
        read_u64(&transaction.open_table(METADATA).unwrap(), "format").unwrap(),
        99
    );
}

#[test]
fn missing_version_tables_are_not_silently_recreated_on_open() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    RedbRecordStore::new(Arc::clone(&database)).unwrap();
    let transaction = database.begin_write().unwrap();
    transaction.delete_table(VERSIONS).unwrap();
    transaction.commit().unwrap();
    assert!(matches!(
        RedbRecordStore::new(Arc::clone(&database)),
        Err(VersionError::InvalidEncoding(_))
    ));
    let transaction = database.begin_read().unwrap();
    assert!(transaction.open_table(VERSIONS).is_err());
}

#[test]
fn an_abort_of_a_known_commit_does_not_attempt_another_physical_commit() {
    let backend = FaultBackend::default();
    let database = Arc::new(
        Database::builder()
            .create_with_backend(backend.clone())
            .unwrap(),
    );
    let store = RedbRecordStore::new(database).unwrap();
    let control = StorageReadControl::with_limit(1024);
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store
        .commit(
            id,
            &PreparedRecordCommit::new(&[], &control).unwrap(),
            &control,
        )
        .unwrap();
    backend.fail_sync.store(true, Ordering::Relaxed);
    assert_eq!(
        store.abort(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert!(backend.fail_sync.swap(false, Ordering::Relaxed));
}

#[test]
fn exhausted_identifiers_and_commit_sequences_never_wrap_or_publish_records() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let store = RedbRecordStore::new(Arc::clone(&database)).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let id = store.allocate_transaction(&control).unwrap();
    let transaction = database.begin_write().unwrap();
    {
        let mut metadata = transaction.open_table(METADATA).unwrap();
        metadata
            .insert("allocated", u64::MAX.to_be_bytes().as_slice())
            .unwrap();
        metadata
            .insert("sequence", u64::MAX.to_be_bytes().as_slice())
            .unwrap();
    }
    transaction.commit().unwrap();
    assert!(matches!(
        store.allocate_transaction(&control),
        Err(VersionError::TransactionIdsExhausted)
    ));
    let prepared = PreparedRecordCommit::new(
        &[uqa_storage::mvcc::RecordWrite {
            key: b"a",
            expected: None,
            value: Some(b"value"),
        }],
        &control,
    )
    .unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::SequenceExhausted))
    ));
    assert!(store
        .snapshot(&control)
        .unwrap()
        .get(b"a", &control)
        .unwrap()
        .is_none());
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
}
