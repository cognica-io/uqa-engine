//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{OwnedBackupJob, OwnedRestoreJob};
use crate::{
    backup::{
        BackupId, BackupLimits, BackupOptions, LocalRepository, Protection, RestoreDestination,
        RestoreOptions, StorageReadControl,
    },
    Engine,
};
use std::{sync::Arc, thread};
use uqa_core::memory::RetainedValue;

#[test]
fn host_jobs_outlive_the_source_and_repository_and_preserve_the_published_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backups");
    let control = StorageReadControl::with_limit(64 << 20);
    let repository = Arc::new(
        LocalRepository::create(
            &path,
            Protection::Plaintext,
            BackupLimits::default(),
            &control,
        )
        .unwrap(),
    );
    let source = Engine::new();
    source
        .sql(
            "CREATE TABLE notes(id integer PRIMARY KEY); INSERT INTO notes VALUES (1)",
            &[],
        )
        .unwrap();
    let parent = OwnedBackupJob::begin(
        &source,
        repository.clone(),
        BackupOptions::default(),
        &control,
    )
    .unwrap()
    .finish()
    .unwrap()
    .backup();
    source.sql("INSERT INTO notes VALUES (2)", &[]).unwrap();
    let expected = source
        .sql("SELECT id FROM notes ORDER BY id", &[])
        .unwrap()
        .rows;
    let options = BackupOptions {
        parent: Some(parent),
        ..BackupOptions::default()
    };
    let mut job = OwnedBackupJob::begin(&source, repository, options, &control).unwrap();
    source.sql("DELETE FROM notes", &[]).unwrap();
    source.close().unwrap();
    drop(source);
    let child = thread::spawn(move || {
        let root = job.finish().unwrap();
        assert_eq!(root.parent(), Some(parent));
        assert_eq!(job.cancel().unwrap().unwrap().backup(), root.backup());
        root.backup()
    })
    .join()
    .unwrap();
    let repository = Arc::new(
        LocalRepository::open(
            &path,
            Protection::Plaintext,
            BackupLimits::default(),
            &control,
        )
        .unwrap(),
    );
    repository.remove_backup(parent, &control).unwrap();
    let mut restore = restore_from_open_statement(repository, child, &control);
    let (first, repeated) = thread::spawn(move || {
        let first = restore.finish().unwrap().into_retained();
        let repeated = restore.cancel().unwrap().unwrap().into_retained();
        (first, repeated)
    })
    .join()
    .unwrap();
    assert!(RetainedValue::ptr_eq(&first, &repeated));
    assert_eq!(
        first
            .sql("SELECT id FROM notes ORDER BY id", &[])
            .unwrap()
            .rows,
        expected
    );
    first.sql("DELETE FROM notes", &[]).unwrap();
    assert!(repeated
        .sql("SELECT id FROM notes", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(control.memory().used() > 0);
    drop((first, repeated));
    assert_eq!(control.memory().used(), 0);
}

fn restore_from_open_statement(
    repository: Arc<LocalRepository>,
    backup: BackupId,
    control: &StorageReadControl,
) -> OwnedRestoreJob {
    let authority = Engine::new();
    // An embedded callback can already own the calling session's statement gate.
    let restore = authority
        .with_direct_query_snapshot(
            true,
            |session| {
                OwnedRestoreJob::begin(
                    session,
                    repository,
                    backup,
                    RestoreDestination::Memory,
                    RestoreOptions::default().with_runtime(session),
                    control,
                )
            },
            std::convert::identity,
        )
        .unwrap();
    authority.close().unwrap();
    restore
}
