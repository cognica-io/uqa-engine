//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use std::{path::Path, sync::Arc};
use uqa_storage::PersistentStorageProvider;

pub(super) fn destination(provider: Provider, path: &Path) -> RestoreDestination {
    use uqa_storage_sqlite::{SQLiteBackupMapping, SQLiteBackupTarget};
    let target: Arc<dyn PersistentBackupTarget> = match provider {
        Provider::Memory => return RestoreDestination::Memory,
        Provider::SQLiteNative => Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::Native)),
        Provider::SQLiteKeyValue => {
            Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::KeyValue))
        }
        Provider::Redb => Arc::new(uqa_storage_redb::RedbBackupTarget::default()),
    };
    RestoreDestination::Persistent {
        directory: path.to_path_buf(),
        target,
    }
}

pub(super) fn capture_provider(
    provider: &dyn PersistentStorageProvider,
    repository: &LocalRepository,
) -> BackupRoot {
    let control = StorageReadControl::with_limit(32 << 20);
    let image = provider.capture_backup_image(&control).unwrap();
    let mut job = repository
        .begin_image_backup(image, None, 32 << 20, 16 << 20, &control)
        .unwrap();
    while job.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(control.memory().used(), 0);
    root
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn public_restore_uses_the_requested_window_for_catalog_validation(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source
        .sql(
            "CREATE TABLE items(id integer PRIMARY KEY, parent integer REFERENCES items(id)); INSERT INTO items VALUES (7, 7)",
            &[],
        )
        .unwrap();
    let root = capture(&source, &repository, None);
    drop(source);

    let calls = [1, 64].map(|window| {
        let path = directory.path().join(format!("target-{window}"));
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = Engine::new()
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &path),
                RestoreOptions::new(32 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        while job.phase() == RestorePhase::Reading {
            job.advance(16 << 10).unwrap();
        }
        let mut calls = 0;
        while job.phase() == RestorePhase::Validating {
            job.advance(window).unwrap();
            calls += 1;
        }
        assert_eq!(job.phase(), RestorePhase::Prepared);
        assert!(!path.exists());
        let engine = job.publish().unwrap();
        assert_eq!(
            engine.sql("SELECT parent FROM items", &[]).unwrap().rows[0]["parent"],
            Value::Int(7)
        );
        drop((engine, job));
        assert_eq!(control.memory().used(), 0);
        calls
    });
    assert!(
        calls[0] > 2 * calls[1],
        "validation calls at 1 and 64 steps: {calls:?}"
    );
}
