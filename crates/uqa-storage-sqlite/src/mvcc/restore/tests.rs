//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{cell::RefCell, path::Path};

use uqa_storage::mvcc::{
    CommitReceipt, CommitSequence, CommitStatus, IdentifierRequest, PreparedRecordCommit,
    RecordWrite, SerializableTransactionId, StorageTransactionId, VersionedPersistence,
    VersionedSessionOptions,
};

use super::*;
use crate::{
    mvcc::native::{NativeRecord, NativeRecordFamily, NativeRecordOwner},
    Catalog, SQLiteCompressionOptions, SQLiteRecordStore,
};

mod failures;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod process;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Boundary {
    IntentPublished,
    CoordinatorPublished,
    Completed,
}

type Hook = (Boundary, Box<dyn FnOnce() -> PhysicalResult<()>>);
thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

struct Injection;
impl Drop for Injection {
    fn drop(&mut self) {
        HOOK.with(|hook| hook.borrow_mut().take());
    }
}

fn inject(at: Boundary, run: impl FnOnce() -> PhysicalResult<()> + 'static) -> Injection {
    HOOK.with(|hook| assert!(hook.borrow_mut().replace((at, Box::new(run))).is_none()));
    Injection
}

pub(super) fn boundary(at: Boundary) -> PhysicalResult<()> {
    let hook = HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        if hook.as_ref().is_some_and(|(expected, _)| *expected == at) {
            hook.take()
        } else {
            None
        }
    });
    match hook {
        Some((_, run)) => run(),
        None => Ok(()),
    }
}

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 24)
}

fn open(path: &Path, mode: usize) -> crate::Result<ManagedConnection> {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "closed-backup"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "closed-backup",
            SQLiteCompressionOptions::default(),
        ),
    }
}

fn restored(
    path: &Path,
    mode: usize,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> crate::Result<ManagedConnection> {
    match mode {
        0 => ManagedConnection::open_restored(path, request, control),
        1 => ManagedConnection::open_encrypted_restored(path, "closed-backup", request, control),
        2 => ManagedConnection::open_compressed_restored(
            path,
            SQLiteCompressionOptions::default(),
            request,
            control,
        ),
        _ => ManagedConnection::open_compressed_encrypted_restored(
            path,
            "closed-backup",
            SQLiteCompressionOptions::default(),
            request,
            control,
        ),
    }
}

fn records(connection: &ManagedConnection, native: bool) -> SQLiteRecordStore {
    if native {
        SQLiteRecordStore::for_native(connection, &control()).unwrap()
    } else {
        SQLiteRecordStore::for_key_value(connection, &control()).unwrap()
    }
}

struct Backup {
    request: DatabaseRestore,
    sequence: CommitSequence,
    receipt: CommitReceipt,
    pending: StorageTransactionId,
    participant: SerializableTransactionId,
    namespace: Option<DatabaseId>,
    coordinator: [u8; 16],
    key: Vec<u8>,
    value: Vec<u8>,
}

fn seed(path: &Path, mode: usize, native: bool) -> Backup {
    let connection = open(path, mode).unwrap();
    if native {
        Catalog::open(connection.clone())
            .unwrap()
            .save_vertex(7, "restored-vertex", "{}")
            .unwrap();
    }
    let store = records(&connection, native);
    let control = control();
    let (key, value) = match store.native_namespace() {
        Some(namespace) => {
            let record = NativeRecord::encode(
                NativeRecordFamily::Metadata,
                NativeRecordOwner::Database(namespace),
                &[
                    rusqlite::types::ValueRef::Text(b"restored-metadata"),
                    rusqlite::types::ValueRef::Text(b"seed-value"),
                ],
                &control,
            )
            .unwrap();
            (record.key().to_vec(), record.row().to_vec())
        }
        None => (b"restored-key".to_vec(), b"seed-value".to_vec()),
    };
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: &key,
            expected: None,
            value: Some(&value),
        }],
        &control,
    )
    .unwrap();
    let transaction = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(transaction, &prepared, &control).unwrap();
    let pending = store.allocate_transaction(&control).unwrap();
    store
        .allocate_identifiers(
            b"restore-watermark",
            IdentifierRequest::Observe(700),
            &control,
        )
        .unwrap();
    let mut auxiliary = store.serializable_admission(&control).unwrap();
    let participant = auxiliary.graph_mut().admit(false, &control).unwrap();
    let coordinator = auxiliary.graph().coordinator();
    auxiliary.persist(&control).unwrap();
    Backup {
        request: DatabaseRestore::new(store.database_id()).unwrap(),
        sequence: store.snapshot(&control).unwrap().sequence(),
        namespace: store.native_namespace(),
        receipt,
        pending,
        participant,
        coordinator,
        key,
        value,
    }
}

fn verify(connection: &ManagedConnection, backup: &Backup, native: bool) {
    let store = records(connection, native);
    let control = control();
    assert_eq!(store.database_id(), backup.request.target());
    assert_eq!(store.native_namespace(), backup.namespace);
    assert_eq!(
        store.snapshot(&control).unwrap().sequence(),
        backup.sequence
    );
    let record = store
        .snapshot(&control)
        .unwrap()
        .get(&backup.key, &control)
        .unwrap()
        .unwrap();
    assert_eq!(record.sequence(), backup.receipt.sequence);
    assert_eq!(&***record.value().unwrap(), &backup.value);
    assert_eq!(
        store
            .identifier_watermark(b"restore-watermark", &control)
            .unwrap(),
        Some(700)
    );
    for transaction in [backup.receipt.transaction, backup.pending] {
        assert!(matches!(
            store.commit_status(transaction, &control),
            Err(VersionError::WrongDatabase)
        ));
    }
    let auxiliary = store.serializable_admission(&control).unwrap();
    assert_eq!(auxiliary.graph().database(), backup.request.target());
    assert_ne!(auxiliary.graph().coordinator(), backup.coordinator);
    assert!(auxiliary.graph().check_active(backup.participant).is_err());
    drop(auxiliary);
    if native {
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        assert_eq!(
            catalog
                .get_metadata("restored-metadata")
                .unwrap()
                .as_deref(),
            Some("seed-value")
        );
        assert_eq!(
            catalog.graph_vertex(7).unwrap().unwrap().label,
            "restored-vertex"
        );
    }
}

#[test]
fn closed_sqlite_backups_change_history_preserve_records_and_keep_completed_retries() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.db");
            let copy = directory.path().join("copy.db");
            let backup = seed(&path, mode, native);
            // A copy has no auxiliary file and still receives a fresh valid coordinator.
            std::fs::copy(&path, &copy).unwrap();
            let control = control();
            let connection = restored(&copy, mode, backup.request, &control).unwrap();
            verify(&connection, &backup, native);
            let store = records(&connection, native);
            let transaction = store.allocate_transaction(&control).unwrap();
            assert!(transaction.allocation() > backup.pending.allocation());
            let prepared = PreparedRecordCommit::new(&[], &control).unwrap();
            let receipt = store.commit(transaction, &prepared, &control).unwrap();
            let mut auxiliary = store.serializable_admission(&control).unwrap();
            let participant = auxiliary.graph_mut().admit(false, &control).unwrap();
            let coordinator = auxiliary.graph().coordinator();
            auxiliary.persist(&control).unwrap();
            drop(store);
            drop(connection);
            let retry = restored(&copy, mode, backup.request, &control).unwrap();
            let store = records(&retry, native);
            assert_eq!(
                store.commit_status(transaction, &control).unwrap(),
                CommitStatus::Committed(receipt)
            );
            let auxiliary = store.serializable_admission(&control).unwrap();
            assert_eq!(auxiliary.graph().coordinator(), coordinator);
            auxiliary.graph().check_active(participant).unwrap();
            drop(auxiliary);
            drop(store);
            drop(retry);
            let reopened = open(&copy, mode).unwrap();
            verify(&reopened, &backup, native);
            drop(reopened);
            // The original closed history and its receipts are unaffected by restoring a copy.
            let original = open(&path, mode).unwrap();
            let source = records(&original, native);
            assert_eq!(source.database_id(), backup.request.source());
            assert_eq!(
                source
                    .commit_status(backup.receipt.transaction, &control)
                    .unwrap(),
                CommitStatus::Committed(backup.receipt)
            );
        }
    }
}
