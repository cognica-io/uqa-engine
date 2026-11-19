//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{path::Path, sync::Arc};
use uqa_storage::{PersistentStorageProvider, StorageEncryptionKey};
use uqa_storage_redb::{RedbBackupTarget, RedbStorage};
use uqa_storage_sqlite::{
    ManagedConnection, SQLiteBackupMapping, SQLiteBackupTarget, SQLiteCompressionOptions,
    SQLiteKeyValueStorage, SQLiteStorageProvider,
};

#[derive(Clone, Copy, Debug)]
enum Provider {
    Native,
    KV,
    Redb,
}

impl Provider {
    fn open(self, path: &Path, mode: usize) -> Arc<dyn PersistentStorageProvider> {
        if matches!(self, Self::Redb) {
            return Arc::new(RedbStorage::open(path).unwrap());
        }
        let connection = match mode {
            0 => ManagedConnection::open(path),
            1 => ManagedConnection::open_encrypted(path, "source secret"),
            2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
            3 => ManagedConnection::open_compressed_encrypted(
                path,
                "source secret",
                SQLiteCompressionOptions::default(),
            ),
            _ => unreachable!(),
        }
        .unwrap();
        match self {
            Self::Native => Arc::new(SQLiteStorageProvider::new(connection)),
            Self::KV => Arc::new(SQLiteKeyValueStorage::from_connection(connection).unwrap()),
            Self::Redb => unreachable!(),
        }
    }

    fn target(self, mode: usize) -> Arc<dyn PersistentBackupTarget> {
        if matches!(self, Self::Redb) {
            return Arc::new(RedbBackupTarget::default());
        }
        let mut target = SQLiteBackupTarget::new(match self {
            Self::Native => SQLiteBackupMapping::Native,
            Self::KV => SQLiteBackupMapping::KeyValue,
            Self::Redb => unreachable!(),
        });
        target.compression = (mode >= 2).then(SQLiteCompressionOptions::default);
        target.encryption_key =
            (mode % 2 == 1).then(|| StorageEncryptionKey::new("destination secret"));
        Arc::new(target)
    }
}

fn protected_repository(path: &Path) -> LocalRepository {
    LocalRepository::create(
        path,
        Protection::Encrypted {
            domain: crate::backup::ProtectionId::from_bytes([37; 16]).unwrap(),
            key: crate::backup::BackupKey::new([47; 32]),
        },
        BackupLimits::new(4096, 4096).unwrap(),
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

fn destination(path: &Path, target: &Arc<dyn PersistentBackupTarget>) -> RestoreDestination {
    RestoreDestination::Persistent {
        directory: path.to_path_buf(),
        target: Arc::clone(target),
    }
}

fn options() -> RestoreOptions {
    RestoreOptions::new(16 << 20, 8 << 20)
}

#[rstest::rstest]
#[case::native(Provider::Native)]
#[case::kv(Provider::KV)]
#[case::redb(Provider::Redb)]
fn persistent_restore_honors_each_calls_object_verification_window(#[case] kind: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source =
        Engine::from_persistent_provider(kind.open(&directory.path().join("source"), 0)).unwrap();
    let value = Value::Str("original payload".repeat(4096));
    source.sql("CREATE TABLE items(body text)", &[]).unwrap();
    source
        .sql(
            "INSERT INTO items VALUES($1)",
            &[uqa_sql::SQLParam::scalar(value.clone())],
        )
        .unwrap();
    let repository = protected_repository(&directory.path().join("repository"));
    let root = capture(&source, &repository, None);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut reader = repository
        .open_backup(root.backup(), &control)
        .unwrap()
        .into_image_reader(16 << 20, &control)
        .unwrap();
    let mut buffer = [0; 4096];
    loop {
        match reader.advance(16 << 10).unwrap() {
            uqa_storage::backup::BackupReadStep::Unit => {
                while reader.read_current(&mut buffer).unwrap() != 0 {}
            }
            uqa_storage::backup::BackupReadStep::Pending => (),
            uqa_storage::backup::BackupReadStep::Complete => break,
        }
    }
    let object_bytes = reader.verified_object_bytes();
    drop(reader);
    let authority = Engine::new();
    let target = kind.target(0);
    for maximum in [17, 4096] {
        let path = directory.path().join(format!("restored-{maximum}"));
        let mut job = authority
            .begin_restore(
                &repository,
                root.backup(),
                destination(&path, &target),
                options(),
                &control,
            )
            .unwrap();
        let mut steps = 0;
        while job.phase() != RestorePhase::Prepared {
            job.advance(maximum).unwrap();
            steps += 1;
            assert!(steps < object_bytes + 4096, "restore stopped progressing");
        }
        assert!(
            steps >= object_bytes.div_ceil(maximum as u64),
            "{object_bytes} verified object bytes exceeded {maximum} bytes in each of {steps} calls"
        );
        let restored = job.publish().unwrap();
        assert_eq!(
            restored.sql("SELECT body FROM items", &[]).unwrap().rows[0]["body"],
            value
        );
        drop((restored, job));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn persistent_restore_admission_applies_authority_transaction_and_target_exclusivity() {
    let directory = tempfile::tempdir().unwrap();
    let repository = protected_repository(&directory.path().join("repository"));
    let source = Engine::from_persistent_provider(
        Provider::Native.open(&directory.path().join("source"), 0),
    )
    .unwrap();
    let root = capture(&source, &repository, None);
    drop(source);
    let path = directory.path().join("restored");
    let target = Provider::Native.target(0);
    let authority = Engine::new();
    let control = StorageReadControl::with_limit(32 << 20);
    let id = RestoreId::new().unwrap();
    authority
        .sql("CREATE ROLE reader; SET ROLE reader", &[])
        .unwrap();
    for expected in ["42501", "25P02"] {
        let begin = authority.begin_restore(
            &repository,
            root.backup(),
            destination(&path, &target),
            options(),
            &control,
        );
        assert_eq!(begin.err().unwrap().sqlstate(), Some(expected));
        let resume = authority.resume_restore(
            &repository,
            id,
            destination(&path, &target),
            options(),
            &control,
        );
        assert_eq!(resume.err().unwrap().sqlstate(), Some(expected));
        let cancel = authority.cancel_restore(
            &repository,
            id,
            destination(&path, &target),
            options(),
            &control,
        );
        assert_eq!(cancel.err().unwrap().sqlstate(), Some(expected));
        assert_eq!(control.memory().used(), 0);
        assert!(!path.exists());
        if expected == "42501" {
            authority.sql("RESET ROLE; BEGIN", &[]).unwrap();
            assert!(authority.sql("SELECT 1/0", &[]).is_err());
        }
    }
    authority.sql("ROLLBACK", &[]).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("sentinel"), b"existing destination").unwrap();
    assert!(authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(&path, &target),
            options(),
            &control
        )
        .is_err());
    assert_eq!(
        std::fs::read(path.join("sentinel")).unwrap(),
        b"existing destination"
    );
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::native(Provider::Native)]
#[case::kv(Provider::KV)]
#[case::redb(Provider::Redb)]
fn persistent_restore_recovers_private_completion_and_retires_failed_or_cancelled_input(
    #[case] kind: Provider,
    #[values("validating", "prepared", "cancel", "failure")] outcome: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let source =
        Engine::from_persistent_provider(kind.open(&directory.path().join("source"), 0)).unwrap();
    source
        .sql(
            "CREATE TABLE items(id integer); INSERT INTO items VALUES(17)",
            &[],
        )
        .unwrap();
    let repository = protected_repository(&directory.path().join("repository"));
    let root = capture(&source, &repository, None);
    drop(source);
    let path = directory.path().join("restored");
    let target = kind.target(0);
    let authority = Engine::new();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(&path, &target),
            options(),
            &control,
        )
        .unwrap();
    let id = job.id().unwrap();
    if matches!(outcome, "validating" | "prepared") {
        while job.advance(1024).unwrap() != RestorePhase::Validating {}
        if outcome == "prepared" {
            while job.advance(1024).unwrap() != RestorePhase::Prepared {}
        }
        drop(job);
        assert_eq!(control.memory().used(), 0);
        if outcome == "prepared" {
            repository.remove_backup(root.backup(), &control).unwrap();
        }
        let mut job = authority
            .resume_restore(
                &repository,
                id,
                destination(&path, &target),
                options(),
                &control,
            )
            .unwrap();
        if outcome == "validating" {
            prepare(&mut job);
        }
        assert_eq!(job.phase(), RestorePhase::Prepared);
        assert!(!path.exists());
        let restored = job.publish().unwrap();
        assert_eq!(
            restored.sql("SELECT id FROM items", &[]).unwrap().rows[0]["id"],
            Value::Int(17)
        );
        drop((restored, job));
    } else {
        // File construction belongs to the first admitted step, so all later failures retain a caller-visible durable identity.
        assert_eq!(job.advance(1024).unwrap(), RestorePhase::Reading);
        control.cancellation().cancel();
        if outcome == "cancel" {
            assert!(job.cancel().unwrap().is_none());
            assert_eq!(job.phase(), RestorePhase::Cancelled);
        } else {
            assert!(matches!(job.advance(1024), Err(BackupError::Cancelled(_))));
            assert_eq!(job.phase(), RestorePhase::Failed);
            assert!(matches!(job.advance(1024), Err(BackupError::Finished)));
        }
        assert_eq!(control.memory().used(), 0);
        drop(job);
        assert!(authority
            .cancel_restore(
                &repository,
                id,
                destination(&path, &target),
                options(),
                &control
            )
            .unwrap()
            .is_none());
        assert!(authority
            .cancel_restore(
                &repository,
                id,
                destination(&path, &target),
                options(),
                &control
            )
            .unwrap()
            .is_none());
        let mut cancelled = authority
            .resume_restore(
                &repository,
                id,
                destination(&path, &target),
                options(),
                &control,
            )
            .unwrap();
        assert_eq!(cancelled.phase(), RestorePhase::Cancelled);
        assert!(matches!(
            cancelled.advance(1024),
            Err(BackupError::Finished)
        ));
        drop(cancelled);
        assert!(!path.exists());
        repository
            .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
            .unwrap();
    }
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::native(Provider::Native)]
#[case::kv(Provider::KV)]
#[case::redb(Provider::Redb)]
fn persistent_restore_rejects_a_real_archive_with_a_missing_view_relation(#[case] kind: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let provider = kind.open(&directory.path().join("source"), 0);
    let source = Engine::from_persistent_provider(Arc::clone(&provider)).unwrap();
    source.sql("CREATE TABLE items(id integer); INSERT INTO items VALUES(17); CREATE VIEW visible AS SELECT id FROM items", &[]).unwrap();
    drop(source);
    let session = provider.open_session().unwrap();
    session.catalog.drop_table("public.items").unwrap();
    drop(session);
    let control = StorageReadControl::with_limit(32 << 20);
    let repository = protected_repository(&directory.path().join("repository"));
    let image = provider.capture_backup_image(&control).unwrap();
    let mut capture = repository
        .begin_image_backup(image, None, 32 << 20, 16 << 20, &control)
        .unwrap();
    while capture.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
    let root = capture.publish().unwrap();
    drop((capture, provider));
    let path = directory.path().join("restored");
    let target = kind.target(0);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(&path, &target),
            options(),
            &control,
        )
        .unwrap();
    let id = job.id().unwrap();
    while job.advance(1024).unwrap() != RestorePhase::Validating {}
    let mut failure = None;
    for _ in 0..65536 {
        match job.advance(1024) {
            Err(error) => {
                failure = Some(error);
                break;
            }
            Ok(RestorePhase::Validating) => (),
            Ok(phase) => panic!("invalid catalog escaped validation: {phase:?}"),
        }
    }
    let error = failure.expect("invalid catalog validation did not finish");
    assert!(error.to_string().contains("items"), "{error}");
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    assert_eq!(control.memory().used(), 0);
    drop(job);
    assert!(authority
        .cancel_restore(
            &repository,
            id,
            destination(&path, &target),
            options(),
            &control
        )
        .unwrap()
        .is_none());
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_eq!(control.memory().used(), 0);
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
fn persistent_restore_publishes_full_and_standalone_incremental_images(
    #[case] kind: Provider,
    #[case] mode: usize,
    #[values(false, true)] incremental: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let source_directory = directory.path().join("source");
    std::fs::create_dir(&source_directory).unwrap();
    let source =
        Engine::from_persistent_provider(kind.open(&source_directory.join("database"), mode))
            .unwrap();
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text); CREATE SEQUENCE ticket START 41; INSERT INTO items VALUES(1,'first'); CREATE VIEW visible AS SELECT id,body FROM items", &[]).unwrap();
    let repository = protected_repository(&directory.path().join("repository"));
    let parent = capture(&source, &repository, None);
    let root = if incremental {
        source
            .sql(
                "UPDATE items SET body='latest'; INSERT INTO items VALUES(2,'second')",
                &[],
            )
            .unwrap();
        capture(&source, &repository, Some(parent.backup()))
    } else {
        parent
    };
    let expected = source
        .sql("SELECT id,body FROM visible ORDER BY id", &[])
        .unwrap()
        .rows;
    drop(source);
    std::fs::remove_dir_all(source_directory).unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    if incremental {
        repository.remove_backup(parent.backup(), &control).unwrap();
        for attempt in 0..100 {
            if repository
                .collect_garbage(16, 16 << 20, &control)
                .unwrap()
                .remaining_files
                == 0
            {
                break;
            }
            assert!(attempt < 99, "ancestor collection did not finish");
        }
    }
    let path = directory.path().join("restored");
    let target = kind.target((mode + usize::from(incremental)) % 4);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(&path, &target),
            options(),
            &control,
        )
        .unwrap();
    drop(authority);
    let id = job.id().unwrap();
    assert_eq!(job.backup(), root.backup());
    assert_eq!(job.root(), Some(root));
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    prepare(&mut job);
    assert!(
        !path.exists(),
        "physical and logical validation keep the target private"
    );
    let restored = job.publish().unwrap();
    assert!(path.join("database").is_file());
    assert_eq!(
        restored
            .sql("SELECT id,body FROM visible ORDER BY id", &[])
            .unwrap()
            .rows,
        expected
    );
    assert_eq!(
        restored
            .sql("SELECT nextval('ticket') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(41)
    );
    restored
        .sql("INSERT INTO items VALUES(3,'after publication')", &[])
        .unwrap();
    restored
        .sql(
            "BEGIN; INSERT INTO items VALUES(4,'rolled back'); ROLLBACK",
            &[],
        )
        .unwrap();
    let late = job.publish().unwrap();
    assert!(std::ptr::eq::<Engine>(
        &raw const *restored,
        &raw const *late
    ));
    drop(late);
    let fresh = capture(&restored, &repository, None);
    assert_ne!(fresh.cut().source, root.cut().source);
    drop(job);
    repository.remove_backup(root.backup(), &control).unwrap();
    // Closing the original runtime permits redb's exclusive physical owner to reopen; recovery must preserve every later committed write and never require the retired archive.
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    let authority = Engine::new();
    control.cancellation().cancel();
    let mut resumed = authority
        .resume_restore(
            &repository,
            id,
            destination(&path, &target),
            options(),
            &control,
        )
        .unwrap();
    assert_eq!(resumed.backup(), root.backup());
    assert_eq!(resumed.root(), None);
    assert_eq!(resumed.phase(), RestorePhase::Prepared);
    let recovered = resumed.publish().unwrap();
    let values = recovered
        .sql("SELECT id FROM items ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(values.len(), if incremental { 3 } else { 2 });
    assert_eq!(values.last().unwrap()["id"], Value::Int(3));
    assert_eq!(
        recovered
            .sql("SELECT nextval('ticket') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(42)
    );
    let late = resumed.cancel().unwrap().unwrap();
    assert!(std::ptr::eq::<Engine>(
        &raw const *recovered,
        &raw const *late
    ));
    drop((late, resumed, authority));
    drop(repository);
    recovered
        .sql(
            "INSERT INTO items VALUES(5,'after repository retirement')",
            &[],
        )
        .unwrap();
    drop(recovered);
    assert_eq!(control.memory().used(), 0);
}
