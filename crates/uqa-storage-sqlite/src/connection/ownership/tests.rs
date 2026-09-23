//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::any::Any;

use uqa_storage::mvcc::VersionedPersistence;
use uqa_storage::PersistentStorageProvider;

use super::*;
use crate::{
    ManagedConnection, SQLiteCompressionOptions, SQLiteRecordStore, SQLiteStorageProvider,
};

fn connection(path: &Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "restore owner"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "restore owner",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

fn retained(path: &Path, mode: usize, kind: usize, control: &StorageReadControl) -> Box<dyn Any> {
    let connection = connection(path, mode);
    let records = SQLiteRecordStore::for_native(&connection, control).unwrap();
    match kind {
        0 => Box::new(connection.clone()),
        1 => Box::new(connection.lease_connection().unwrap()),
        2 => Box::new(records),
        3 => Box::new(records.snapshot(control).unwrap()),
        4 => Box::new(
            records
                .admit_serializable(true, control, || Ok(()))
                .unwrap()
                .0,
        ),
        5 => Box::new(records.serializable_admission(control).unwrap()),
        6 => Box::new(SQLiteStorageProvider::new(connection)),
        _ => Box::new(
            SQLiteStorageProvider::new(connection)
                .open_session()
                .unwrap(),
        ),
    }
}

#[test]
fn every_retained_sqlite_owner_excludes_restore_until_final_release() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    for mode in 0..4 {
        for kind in 0..8 {
            let path = directory.path().join(format!("owner-{mode}-{kind}.db"));
            let held = retained(&path, mode, kind, &control);
            assert!(
                matches!(
                    RestoreAdmission::acquire(&path, &control),
                    Err(SQLiteError::DatabaseRestoreBusy)
                ),
                "mode {mode}, owner {kind}"
            );
            drop(held);
            let admission = RestoreAdmission::acquire(&path, &control).unwrap();
            let owner = admission.retain(&control).unwrap();
            drop(admission);
            assert!(matches!(
                RestoreAdmission::acquire(&path, &control),
                Err(SQLiteError::DatabaseRestoreBusy)
            ));
            drop(owner);
            drop(RestoreAdmission::acquire(&path, &control).unwrap());
        }
    }
}

#[test]
fn failed_restore_owner_retention_releases_admission_without_creating_a_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.db");
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(RestoreAdmission::acquire(&path, &control).is_err());
    assert!(!path.exists());
    drop(connection(&path, 0));
    let admission = RestoreAdmission::acquire(&path, &control).unwrap();
    assert!(matches!(
        admission.retain(&StorageReadControl::with_limit(0)),
        Err(SQLiteError::Memory(_))
    ));
    drop(admission);
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(matches!(
        RestoreAdmission::acquire(&path, &cancelled),
        Err(SQLiteError::Cancelled(_))
    ));
    drop(connection(&path, 0));
    drop(RestoreAdmission::acquire(&path, &control).unwrap());
}

#[test]
fn local_file_owners_exclude_restore_and_handoff_without_losing_admission() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("local.db");
    let control = StorageReadControl::with_limit(1 << 20);
    let shared = local::Admission::acquire(&path, false, &control).unwrap();
    let lease = shared.retain(&control).unwrap();
    drop(shared);
    assert!(matches!(
        local::Admission::acquire(&path, true, &control),
        Err(SQLiteError::DatabaseRestoreBusy)
    ));
    drop(lease);
    let exclusive = local::Admission::acquire(&path, true, &control).unwrap();
    assert!(matches!(
        local::Admission::acquire(&path, false, &control),
        Err(SQLiteError::DatabaseRestoreBusy)
    ));
    let lease = exclusive.retain(&control).unwrap();
    drop(exclusive);
    assert!(matches!(
        local::Admission::acquire(&path, true, &control),
        Err(SQLiteError::DatabaseRestoreBusy)
    ));
    drop(lease);
    drop(local::Admission::acquire(&path, true, &control).unwrap());
    assert_eq!(control.memory().used(), 0);
}
