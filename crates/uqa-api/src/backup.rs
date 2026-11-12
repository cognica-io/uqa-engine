//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded backup jobs share the Engine's authority and provider lifecycle.

pub use uqa_engine::backup::*;

/// Cancel the same durable request after an interrupted or failed job, preserving a completed destination's later writes.
pub fn cancel_restore(
    engine: &uqa_engine::Engine,
    repository: &LocalRepository,
    id: RestoreId,
    destination: RestoreDestination,
    options: RestoreOptions,
    control: &StorageReadControl,
) -> Result<Option<RestoredEngine>, uqa_sql::SQLError> {
    engine.cancel_restore(repository, id, destination, options, control)
}

/// Resolve the Engine's exact durable restore request without replaying a completed import.
pub fn resume_restore<'a>(
    engine: &uqa_engine::Engine,
    repository: &'a LocalRepository,
    id: RestoreId,
    destination: RestoreDestination,
    options: RestoreOptions,
    control: &StorageReadControl,
) -> Result<RestoreJob<'a>, uqa_sql::SQLError> {
    engine.resume_restore(repository, id, destination, options, control)
}

/// Start the same authorized restoration exposed by [`uqa_engine::Engine::begin_restore`].
pub fn begin_restore<'a>(
    engine: &uqa_engine::Engine,
    repository: &'a LocalRepository,
    backup: BackupId,
    destination: RestoreDestination,
    options: RestoreOptions,
    control: &StorageReadControl,
) -> Result<RestoreJob<'a>, uqa_sql::SQLError> {
    engine.begin_restore(repository, backup, destination, options, control)
}

/// Start the same authorized, bounded persistent export exposed by [`uqa_engine::Engine::begin_backup`].
pub fn begin_backup<'a>(
    engine: &uqa_engine::Engine,
    repository: &'a LocalRepository,
    options: BackupOptions,
    control: &StorageReadControl,
) -> Result<BackupJob<'a>, uqa_sql::SQLError> {
    engine.begin_backup(repository, options, control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Provider {
        Native,
        KeyValue,
        Redb,
    }

    impl Provider {
        fn open(self, path: &std::path::Path) -> uqa_engine::Engine {
            match self {
                Self::Native => uqa_engine::Engine::open(path).unwrap(),
                Self::KeyValue => {
                    uqa_engine::Engine::from_persistent_provider(std::sync::Arc::new(
                        uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
                    ))
                    .unwrap()
                }
                Self::Redb => uqa_engine::Engine::from_persistent_provider(std::sync::Arc::new(
                    uqa_storage_redb::RedbStorage::open(path).unwrap(),
                ))
                .unwrap(),
            }
        }

        fn destination(self, directory: std::path::PathBuf) -> RestoreDestination {
            RestoreDestination::Persistent {
                directory,
                target: match self {
                    Self::Native => {
                        std::sync::Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::Native))
                    }
                    Self::KeyValue => {
                        std::sync::Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::KeyValue))
                    }
                    Self::Redb => {
                        std::sync::Arc::new(uqa_storage_redb::RedbBackupTarget::default())
                    }
                },
            }
        }
    }

    #[test]
    fn api_persistent_restore_resolves_completion_and_cancellation_without_archive_replay() {
        persistent_restore(Provider::Native);
    }

    #[test]
    fn api_kv_restore_resolves_completion_and_cancellation_without_archive_replay() {
        persistent_restore(Provider::KeyValue);
    }

    #[test]
    fn api_redb_restore_resolves_completion_and_cancellation_without_archive_replay() {
        persistent_restore(Provider::Redb);
    }

    fn persistent_restore(provider: Provider) {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let repository = LocalRepository::create(
            directory.path().join("backups"),
            Protection::Plaintext,
            BackupLimits::new(4096, 4096).unwrap(),
            &control,
        )
        .unwrap();
        let source = provider.open(&directory.path().join("source"));
        source
            .sql(
                "CREATE TABLE items(value integer); INSERT INTO items VALUES(7)",
                &[],
            )
            .unwrap();
        let mut capture = begin_backup(
            &source,
            &repository,
            BackupOptions::new(16 << 20, 16 << 20),
            &control,
        )
        .unwrap();
        while capture.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
        let root = capture.publish().unwrap();
        drop((capture, source));
        let authority = uqa_engine::Engine::new();
        let target = provider.destination(directory.path().join("restored"));
        let options = RestoreOptions::new(16 << 20, 8 << 20);
        let mut job = begin_restore(
            &authority,
            &repository,
            root.backup(),
            target.clone(),
            options,
            &control,
        )
        .unwrap();
        let id = job.id().unwrap();
        while job.advance(4096).unwrap() != RestorePhase::Prepared {}
        let restored = job.publish().unwrap();
        restored.sql("INSERT INTO items VALUES(8)", &[]).unwrap();
        drop((restored, job));
        repository.remove_backup(root.backup(), &control).unwrap();
        let mut resumed = resume_restore(
            &authority,
            &repository,
            id,
            target.clone(),
            options,
            &control,
        )
        .unwrap();
        let restored = resumed.publish().unwrap();
        assert_eq!(
            restored
                .sql("SELECT value FROM items ORDER BY value", &[])
                .unwrap()
                .rows
                .len(),
            2
        );
        drop((restored, resumed));
        control.cancellation().cancel();
        let completed = cancel_restore(&authority, &repository, id, target, options, &control)
            .unwrap()
            .unwrap();
        drop((authority, repository));
        completed.sql("INSERT INTO items VALUES(9)", &[]).unwrap();
        drop(completed);
        assert_eq!(control.memory().used(), 0);
    }

    #[test]
    fn api_restore_job_retains_the_activated_engine_after_all_job_handles_close() {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let repository = LocalRepository::create(
            directory.path().join("backups"),
            Protection::Plaintext,
            BackupLimits::new(4096, 4096).unwrap(),
            &control,
        )
        .unwrap();
        let source = uqa_engine::Engine::new();
        source
            .sql(
                "CREATE TABLE items(value integer); INSERT INTO items VALUES(7)",
                &[],
            )
            .unwrap();
        let mut capture = begin_backup(
            &source,
            &repository,
            BackupOptions::new(16 << 20, 16 << 20),
            &control,
        )
        .unwrap();
        drop(source);
        while capture.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
        let root = capture.publish().unwrap();
        drop(capture);
        let authority = uqa_engine::Engine::new();
        let mut restore = begin_restore(
            &authority,
            &repository,
            root.backup(),
            RestoreDestination::Memory,
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
        drop(authority);
        while restore.advance(4096).unwrap() != RestorePhase::Prepared {}
        let engine = restore.publish().unwrap();
        drop(restore);
        drop(repository);
        assert_eq!(
            engine.sql("SELECT value FROM items", &[]).unwrap().rows[0]["value"],
            uqa_core::Value::Int(7)
        );
        engine.sql("INSERT INTO items VALUES(8)", &[]).unwrap();
        drop(engine);
        assert_eq!(control.memory().used(), 0);
    }

    #[test]
    fn api_backup_example_runs_after_engine_retirement() {
        let directory = tempfile::tempdir().unwrap();
        let engine = uqa_engine::Engine::open(&directory.path().join("database")).unwrap();
        engine
            .sql("CREATE TABLE documents (value INTEGER)", &[])
            .unwrap();
        engine.sql("INSERT INTO documents VALUES (7)", &[]).unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let repository = LocalRepository::create(
            directory.path().join("backups"),
            Protection::Plaintext,
            BackupLimits::new(4096, 4096).unwrap(),
            &control,
        )
        .unwrap();
        let mut job = begin_backup(
            &engine,
            &repository,
            BackupOptions::new(16 << 20, 16 << 20),
            &control,
        )
        .unwrap();
        drop(engine);
        while job.advance(128, 16 << 10).unwrap() == BackupCaptureStep::Pending {}
        let root = job.publish().unwrap();
        assert!(root.summary().units > 0);
        drop(job);
        repository
            .open_backup(root.backup(), &control)
            .unwrap()
            .verify(16 << 20, &control)
            .unwrap();
        assert_eq!(control.memory().used(), 0);
    }
}
