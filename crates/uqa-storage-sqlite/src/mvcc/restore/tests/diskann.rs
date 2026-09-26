//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{path::Path, sync::Arc};

use super::{control, open, records, restored};
use crate::{ManagedConnection, SQLiteKeyValueStore, SQLiteStorageProvider};
use uqa_storage::{
    diskann_index::format::DiskANNGeneration,
    key_value::conformance::{
        diskann_restore_records, verify_diskann_restore_source, verify_diskann_restored,
        verify_diskann_restored_rebuild, verify_diskann_restored_writes, DiskANNRestoreRecords,
    },
    mvcc::{DatabaseRestore, VersionedPersistence},
    KeyValueStorageBackend, PersistentStorageBackend, PersistentStorageProvider,
};

struct DiskANNBackup {
    request: DatabaseRestore,
    generation: DiskANNGeneration,
    image: DiskANNRestoreRecords,
}

fn backend(connection: &ManagedConnection, native: bool) -> Arc<dyn PersistentStorageBackend> {
    if native {
        SQLiteStorageProvider::new(connection.clone())
            .open_session()
            .unwrap()
            .backend
    } else {
        Arc::new(KeyValueStorageBackend::new(Arc::new(
            SQLiteKeyValueStore::new(connection.clone()).unwrap(),
        )))
    }
}

fn seed(path: &Path, mode: usize, native: bool) -> DiskANNBackup {
    let connection = open(path, mode).unwrap();
    let backend = backend(&connection, native);
    let generation = verify_diskann_restore_source(&*backend).unwrap();
    let records = records(&connection, native);
    DiskANNBackup {
        request: DatabaseRestore::new(records.database_id()).unwrap(),
        generation,
        image: diskann_restore_records(&records).unwrap(),
    }
}

fn verify(connection: &ManagedConnection, native: bool, backup: &DiskANNBackup) {
    let records = records(connection, native);
    assert_eq!(records.database_id(), backup.request.target());
    assert_eq!(diskann_restore_records(&records).unwrap(), backup.image);
    verify_diskann_restored(&*backend(connection, native), backup.generation).unwrap();
    assert_eq!(diskann_restore_records(&records).unwrap(), backup.image);
}

#[test]
fn diskann_backup_restore_preserves_records_changes_and_new_history_writes() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("diskann-source.db");
            let copy = directory.path().join("diskann-copy.db");
            let backup = seed(&path, mode, native);
            std::fs::copy(&path, &copy).unwrap();
            let (generation, image) = {
                let connection = restored(&copy, mode, backup.request, &control()).unwrap();
                verify(&connection, native, &backup);
                let generation = verify_diskann_restored_writes(
                    &*backend(&connection, native),
                    backup.generation,
                )
                .unwrap();
                (
                    generation,
                    diskann_restore_records(&records(&connection, native)).unwrap(),
                )
            };
            {
                let retry = restored(&copy, mode, backup.request, &control()).unwrap();
                let records = records(&retry, native);
                assert_eq!(records.database_id(), backup.request.target());
                assert_eq!(diskann_restore_records(&records).unwrap(), image);
                verify_diskann_restored_rebuild(&*backend(&retry, native), generation).unwrap();
            }
            {
                let reopened = open(&copy, mode).unwrap();
                assert_eq!(
                    diskann_restore_records(&records(&reopened, native)).unwrap(),
                    image
                );
                verify_diskann_restored_rebuild(&*backend(&reopened, native), generation).unwrap();
            }
            let original = open(&path, mode).unwrap();
            let records = records(&original, native);
            assert_eq!(records.database_id(), backup.request.source());
            assert_eq!(diskann_restore_records(&records).unwrap(), backup.image);
            verify_diskann_restored(&*backend(&original, native), backup.generation).unwrap();
        }
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn diskann_backup_restore_resumes_after_process_loss_at_durable_boundaries() {
    for mode in 0..4 {
        for native in [false, true] {
            for action in ["intent", "coordinator"] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("diskann-interrupted.db");
                let backup = seed(&path, mode, native);
                super::process::ReadyPeer::start(&path, mode, action, backup.request).crash();
                assert!(matches!(
                    open(&path, mode),
                    Err(crate::SQLiteError::DatabaseRestoreIncomplete)
                ));
                let connection = restored(&path, mode, backup.request, &control()).unwrap();
                verify(&connection, native, &backup);
            }
        }
    }
}
