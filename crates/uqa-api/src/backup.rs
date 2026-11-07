//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded backup jobs share the Engine's authority and provider lifecycle.

pub use uqa_engine::backup::*;

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
