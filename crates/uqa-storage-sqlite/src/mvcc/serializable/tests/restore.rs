//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A restored main file cannot borrow terminal receipts from a newer auxiliary history.

use uqa_storage::mvcc::{DatabaseId, SerializableCoordinator};

use super::*;
use crate::{
    mvcc::native::{NativeRecord, NativeRecordFamily, NativeRecordOwner},
    Catalog,
};

fn store(path: &Path, mode: usize, native: bool) -> SQLiteRecordStore {
    let connection = open(path, mode);
    if native {
        if !SQLiteRecordStore::has_native_mapping(&connection.lease_connection().unwrap()).unwrap()
        {
            Catalog::open(connection.clone()).unwrap();
        }
        SQLiteRecordStore::for_native(&connection, &control()).unwrap()
    } else {
        SQLiteRecordStore::for_key_value(&connection, &control()).unwrap()
    }
}

fn changed_record(
    database: DatabaseId,
    native: bool,
    control: &StorageReadControl,
) -> (Vec<u8>, Vec<u8>) {
    if !native {
        return (b"restored-history".to_vec(), b"newer-value".to_vec());
    }
    let record = NativeRecord::encode(
        NativeRecordFamily::Metadata,
        NativeRecordOwner::Database(database),
        &[
            rusqlite::types::ValueRef::Text(b"restored-history"),
            rusqlite::types::ValueRef::Text(b"newer-value"),
        ],
        control,
    )
    .unwrap();
    (record.key().to_vec(), record.row().to_vec())
}

#[test]
fn restoring_a_closed_main_backup_rejects_a_newer_terminal_checkpoint_before_capture() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("restored.db");
            let backup = directory.path().join("closed-backup.db");
            let control = control();
            let identity = {
                let store = store(&path, mode, native);
                store
                    .serializable_admission(&control)
                    .unwrap()
                    .persist(&control)
                    .unwrap();
                store.database_id()
            };
            // Every main and auxiliary owner is closed before this generic file copy.
            std::fs::copy(&path, &backup).unwrap();
            let (publication, receipt, key) = {
                let store = store(&path, mode, native);
                let (participant, snapshot) =
                    store.admit_serializable_snapshot(false, &control).unwrap();
                let (key, value) = changed_record(identity, native, &control);
                assert!(snapshot.get(&key, &control).unwrap().is_none());
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
                let publication = {
                    let mut held = store.serializable_admission(&control).unwrap();
                    let publication = held
                        .graph_mut()
                        .prepare_publication(
                            participant.id(),
                            transaction,
                            prepared.fingerprint(),
                            &control,
                        )
                        .unwrap();
                    held.persist(&control).unwrap();
                    publication
                };
                let mut held = store.serializable_admission(&control).unwrap();
                let receipt = store.commit(transaction, &prepared, &control).unwrap();
                assert_eq!(
                    held.graph_mut()
                        .resolve_publication(publication, CommitStatus::Committed(receipt))
                        .unwrap(),
                    CommitStatus::Committed(receipt)
                );
                held.persist(&control).unwrap();
                (publication, receipt, key)
            };
            assert_eq!(control.memory().used(), 0);
            // Restore the older main file while retaining the newer sidecar at this path.
            std::fs::copy(&backup, &path).unwrap();
            let restored = store(&path, mode, native);
            assert_eq!(restored.database_id(), identity);
            assert_eq!(
                restored
                    .commit_status(receipt.transaction, &control)
                    .unwrap(),
                CommitStatus::Unknown
            );
            assert!(restored
                .snapshot(&control)
                .unwrap()
                .get(&key, &control)
                .unwrap()
                .is_none());
            let mut captured = false;
            let result = restored.admit_serializable(true, &control, || {
                captured = true;
                restored.snapshot(&control)
            });
            assert!(
                matches!(result, Err(VersionError::UnknownTransaction)) && !captured,
                "mode {mode}, native {native}: admitted a restored main history whose newer auxiliary publication {publication:?} is absent from the authoritative receipts"
            );
        }
    }
}
