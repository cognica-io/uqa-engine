//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::backup::{
    BackupCapabilities, BackupCapturePhase, BackupError, BackupJob, BackupOptions,
};

fn repository(path: &Path, control: &StorageReadControl, encrypted: bool) -> LocalRepository {
    LocalRepository::create(
        path,
        if encrypted {
            Protection::Encrypted {
                domain: ProtectionId::from_bytes([17; 16]).unwrap(),
                key: BackupKey::new([19; 32]),
            }
        } else {
            Protection::Plaintext
        },
        BackupLimits::new(4096, 4096).unwrap(),
        control,
    )
    .unwrap()
}

fn finish(mut job: BackupJob<'_>, mut writer: Option<&Engine>) -> BackupRoot {
    while job.advance(7, 1024).unwrap() == BackupCaptureStep::Pending {
        if job.progress().source.copied_payload_bytes > 0 {
            if let Some(writer) = writer.take() {
                assert_eq!(job.progress().phase, BackupCapturePhase::Capturing);
                sql(
                    writer,
                    "UPDATE archive.docs SET title = 'after public cut' WHERE id = 1",
                );
            }
        }
    }
    assert!(writer.is_none(), "writer ran between source windows");
    assert_eq!(job.progress().phase, BackupCapturePhase::Prepared);
    let root = job.publish().unwrap();
    assert_eq!(job.publish().unwrap(), root);
    assert_eq!(job.completed(), Some(root));
    root
}

#[rstest::rstest]
#[case::native_plain(Provider::Native, 0)]
#[case::native_encrypted(Provider::Native, 1)]
#[case::native_compressed(Provider::Native, 2)]
#[case::native_compressed_encrypted(Provider::Native, 3)]
#[case::kv_plain(Provider::KV, 0)]
#[case::kv_encrypted(Provider::KV, 1)]
#[case::kv_compressed(Provider::KV, 2)]
#[case::kv_compressed_encrypted(Provider::KV, 3)]
#[case::redb(Provider::Redb, 0)]
fn public_backup_jobs_preserve_committed_images_after_engine_and_source_loss(
    #[case] kind: Provider,
    #[case] mode: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let provider = kind.open(&source.join("database"), mode);
    let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
    assert_eq!(
        engine.backup_capabilities(),
        BackupCapabilities {
            export: true,
            import: false,
            snapshot_fingerprints: false,
        }
    );
    setup(&engine);
    let original = observe(&engine, provider.as_ref());
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = repository(&directory.path().join("repository"), &control, true);
    let options = BackupOptions::new(32 << 20, 16 << 20);
    sql(&engine, "BEGIN");
    sql(
        &engine,
        "UPDATE archive.docs SET title = 'private' WHERE id = 2",
    );
    sql(
        &engine,
        "CREATE TABLE archive.private_definition (value INTEGER)",
    );
    let job = engine.begin_backup(&repository, options, &control).unwrap();
    assert_eq!(job.cut().format.provider(), kind.image());
    assert_eq!(engine.transaction_depth(), 1);
    assert_eq!(
        sql(&engine, "SELECT title FROM archive.docs WHERE id = 2")[0]["title"],
        Value::Str("private".into())
    );
    sql(&engine, "ROLLBACK");
    let writer = Engine::from_persistent_provider(provider.clone()).unwrap();
    let full = finish(job, Some(&writer));
    let changed = observe(&writer, provider.as_ref());
    assert_ne!(original, changed);
    let child_job = engine
        .begin_backup(
            &repository,
            BackupOptions {
                parent: Some(full.backup()),
                ..options
            },
            &control,
        )
        .unwrap();
    drop((engine, writer, provider));
    let child = finish(child_job, None);
    assert_eq!(control.memory().used(), 0);
    std::fs::remove_dir_all(source).unwrap();
    restore::verify(
        &repository,
        full,
        &directory.path().join("full"),
        kind,
        mode,
        &original,
        &control,
    );
    repository.remove_backup(full.backup(), &control).unwrap();
    restore::verify(
        &repository,
        child,
        &directory.path().join("child"),
        kind,
        (mode + 1) % 4,
        &changed,
        &control,
    );
}

#[test]
fn public_backup_admission_preserves_authority_and_failed_transaction_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("database")).unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let path = directory.path().join("repository");
    let repository = repository(&path, &control, false);
    let options = BackupOptions::new(16 << 20, 16 << 20);
    sql(&engine, "CREATE ROLE restricted");
    sql(&engine, "SET ROLE restricted");
    let error = engine
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("42501"));
    sql(&engine, "RESET ROLE");
    sql(&engine, "BEGIN");
    assert!(engine.sql("SELECT 1 / 0", &[]).is_err());
    let error = engine
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("25P02"));
    assert_eq!(
        std::fs::read_dir(path.join("operations")).unwrap().count(),
        0
    );
    sql(&engine, "ROLLBACK");
    let denied = StorageReadControl::with_limit(0);
    let error = engine
        .begin_backup(&repository, options, &denied)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(denied.memory().used(), 0);
    assert_eq!(engine.transaction_depth(), 0);
    let mut job = engine.begin_backup(&repository, options, &control).unwrap();
    assert!(job.cancel().unwrap().is_none());
    drop(job);
    assert_eq!(control.memory().used(), 0);

    let memory = Engine::new();
    assert_eq!(
        memory.backup_capabilities(),
        BackupCapabilities::UNSUPPORTED
    );
    let error = memory
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert!(matches!(
        error,
        uqa_sql::SQLError::Host(ref source)
            if matches!(source.downcast_ref::<BackupError>(), Some(BackupError::Unsupported(_)))
    ));
}

#[rstest::rstest]
#[case::native(Provider::Native)]
#[case::kv(Provider::KV)]
#[case::redb(Provider::Redb)]
fn public_backup_repository_admission_failure_preserves_the_source_transaction(
    #[case] kind: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let provider = kind.open(&directory.path().join("database"), 0);
    let engine = Engine::from_persistent_provider(provider).unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = repository(&directory.path().join("repository"), &control, false);
    sql(&engine, "CREATE TABLE values_to_keep (value INTEGER)");
    sql(&engine, "BEGIN");
    sql(&engine, "INSERT INTO values_to_keep VALUES (42)");
    let error = engine
        .begin_backup(&repository, BackupOptions::new(1 << 20, 1), &control)
        .err()
        .unwrap();
    assert!(
        matches!(error, uqa_sql::SQLError::Host(ref source) if matches!(source.downcast_ref::<BackupError>(), Some(BackupError::Limit { resource: "capture workspace", .. })))
    );
    assert_eq!(engine.transaction_depth(), 1);
    assert_eq!(
        sql(&engine, "SELECT value FROM values_to_keep")[0]["value"],
        Value::Int(42)
    );
    sql(&engine, "COMMIT");
    assert_eq!(
        sql(&engine, "SELECT value FROM values_to_keep")[0]["value"],
        Value::Int(42)
    );
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::native_encrypted(Provider::Native, 1)]
#[case::native_compressed_encrypted(Provider::Native, 3)]
#[case::kv_encrypted(Provider::KV, 1)]
#[case::kv_compressed_encrypted(Provider::KV, 3)]
fn public_backup_requires_explicit_plaintext_export(#[case] kind: Provider, #[case] mode: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine =
        Engine::from_persistent_provider(kind.open(&directory.path().join("source"), mode))
            .unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let path = directory.path().join("repository");
    let repository = repository(&path, &control, false);
    assert!(repository.protection_domain().is_none());
    let options = BackupOptions::new(16 << 20, 16 << 20);
    let error = engine
        .begin_backup(&repository, options, &control)
        .err()
        .unwrap();
    assert!(matches!(
        error,
        uqa_sql::SQLError::Host(ref source)
            if matches!(source.downcast_ref::<BackupError>(), Some(BackupError::Invalid(_)))
    ));
    assert_eq!(
        std::fs::read_dir(path.join("operations")).unwrap().count(),
        0
    );
    let job = engine
        .begin_backup(
            &repository,
            BackupOptions {
                allow_plaintext_export: true,
                ..options
            },
            &control,
        )
        .unwrap();
    drop(engine);
    let root = finish(job, None);
    repository
        .open_backup(root.backup(), &control)
        .unwrap()
        .verify(16 << 20, &control)
        .unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn public_backup_retains_job_cancellation_after_engine_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("source")).unwrap();
    sql(&engine, "CREATE TABLE rows (id INTEGER)");
    sql(&engine, "INSERT INTO rows VALUES (7)");
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = repository(&directory.path().join("repository"), &control, false);
    let mut job = engine
        .begin_backup(
            &repository,
            BackupOptions::new(16 << 20, 16 << 20),
            &control,
        )
        .unwrap();
    drop(engine);
    assert_eq!(job.advance(1, 1).unwrap(), BackupCaptureStep::Pending);
    control.cancellation().cancel();
    assert!(matches!(job.advance(1, 1), Err(BackupError::Cancelled(_))));
    assert_eq!(job.progress().phase, BackupCapturePhase::Failed);
    assert!(job.advance(1, 1).is_err());
    assert!(job.publish().is_err());
    drop(job);
    assert_eq!(control.memory().used(), 0);
}
