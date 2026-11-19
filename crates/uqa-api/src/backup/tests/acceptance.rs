//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use self::ProtectionChange::{Disable, Enable, Rekey};
use super::providers::{
    FileMode::{self, Compressed, CompressedEncrypted, Encrypted, Plain},
    Provider::{self, KeyValue, Memory, Native, Redb},
};
use crate::backup::*;
use std::path::Path;
use uqa_core::Value;
use uqa_engine::Engine;

const WORKSPACE: u64 = 16 << 20;

// Ordered rows and the live, recovered and clean-reopened sequence positions were also observed on PostgreSQL 18.4.
struct Expected {
    rows: &'static [(i64, &'static str)],
    next_ticket: i64,
}

const ORIGINAL: Expected = Expected {
    rows: &[(1, "first"), (2, "second")],
    next_ticket: 40,
};
#[derive(Clone, Copy, Debug)]
enum ProtectionChange {
    Enable,
    Disable,
    Rekey,
}

fn encrypted(id: u8) -> Protection {
    Protection::Encrypted {
        domain: ProtectionId::from_bytes([id; 16]).unwrap(),
        key: BackupKey::new([id; 32]),
    }
}

impl ProtectionChange {
    fn original(self) -> Protection {
        match self {
            Self::Enable => Protection::Plaintext,
            Self::Disable | Self::Rekey => encrypted(11),
        }
    }

    fn destination(self) -> Protection {
        match self {
            Self::Disable => Protection::Plaintext,
            Self::Enable | Self::Rekey => encrypted(23),
        }
    }
}

fn capture(
    engine: &Engine,
    repository: &LocalRepository,
    parent: Option<BackupId>,
    control: &StorageReadControl,
) -> BackupRoot {
    let options = BackupOptions {
        parent,
        ..BackupOptions::new(WORKSPACE, WORKSPACE)
    };
    let mut capture = begin_backup(engine, repository, options, control).unwrap();
    while capture.advance(128, 16 << 10).unwrap() == BackupCaptureStep::Pending {}
    let report = capture.report().unwrap();
    if parent.is_some() {
        assert!(report.image.reused_units > 0 && report.image.written_units > 0);
    }
    let root = capture.publish().unwrap();
    assert_eq!(root.parent(), parent);
    root
}

fn rows(engine: &Engine) -> Vec<(Value, Value)> {
    engine
        .sql("SELECT id, body FROM visible ORDER BY id", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|mut row| (row.remove("id").unwrap(), row.remove("body").unwrap()))
        .collect()
}

fn next_ticket(engine: &Engine) -> Value {
    engine
        .sql("SELECT nextval('ticket') AS value", &[])
        .unwrap()
        .rows[0]["value"]
        .clone()
}

fn recovered_ticket(source_directory: &Path, provider: Provider, mode: FileMode) -> i64 {
    if matches!(provider, Memory) {
        return 42;
    }
    // All writes and capture jobs have finished and the source is still open. Reopening a copy of its files recovers the durable cut independently of the backup and restore code.
    let cold = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(source_directory).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        std::fs::copy(entry.path(), cold.path().join(entry.file_name())).unwrap();
    }
    let reopened = provider.open(&cold.path().join("database"), mode);
    let rows = reopened
        .sql("SELECT last_value, log_cnt FROM ticket", &[])
        .unwrap()
        .rows;
    assert_eq!(
        (rows[0]["last_value"].clone(), rows[0]["log_cnt"].clone()),
        (Value::Int(72), Value::Int(0))
    );
    assert_eq!(next_ticket(&reopened), Value::Int(73));
    73
}

fn check_restored(
    repository: &LocalRepository,
    root: BackupRoot,
    directory: &Path,
    provider: Provider,
    mode: FileMode,
    expected: &Expected,
    control: &StorageReadControl,
) {
    let authority = Engine::new();
    let destination = provider.destination(directory.to_owned(), mode);
    let options = RestoreOptions::new(WORKSPACE, 8 << 20);
    repository
        .open_backup(root.backup(), control)
        .unwrap()
        .verify(WORKSPACE, control)
        .unwrap();
    let mut job = begin_restore(
        &authority,
        repository,
        root.backup(),
        destination,
        options,
        control,
    )
    .unwrap();
    while job.advance(16 << 10).unwrap() != RestorePhase::Prepared {}
    assert!(!directory.exists());
    let restored = job.publish().unwrap();
    drop((job, authority));
    let next = expected.next_ticket;
    let mut expected: Vec<_> = expected
        .rows
        .iter()
        .map(|(id, body)| (Value::Int(*id), Value::Str((*body).to_owned())))
        .collect();
    assert_eq!(rows(&restored), expected);
    assert_eq!(next_ticket(&restored), Value::Int(next));
    restored
        .sql(
            "BEGIN; INSERT INTO items VALUES(77,'private'); ROLLBACK",
            &[],
        )
        .unwrap();
    restored
        .sql("INSERT INTO items VALUES(99,'new')", &[])
        .unwrap();
    expected.push((Value::Int(99), Value::Str("new".to_owned())));
    assert_eq!(rows(&restored), expected);
    drop(restored);
    if let Some(reopened) = provider.reopen(directory, mode) {
        assert_eq!(rows(&reopened), expected);
        assert_eq!(next_ticket(&reopened), Value::Int(next + 1));
    }
}

fn standalone_copy(
    source: &LocalRepository,
    destination: &LocalRepository,
    child: BackupRoot,
    control: &StorageReadControl,
) -> BackupRoot {
    let lease = source.open_backup(child.backup(), control).unwrap();
    let mut copy = destination
        .begin_backup(child.cut(), None, WORKSPACE, control)
        .unwrap();
    let report = copy.prepare_copy(&lease, WORKSPACE, control).unwrap();
    assert_eq!(report.inventory, child.summary());
    let copied = copy.publish(control).unwrap();
    assert_eq!(copied.summary(), child.summary());
    assert_eq!(copied.cut(), child.cut());
    assert_eq!(copied.parent(), None);
    copied
}

#[rstest::rstest]
#[case::memory(Memory, Plain, Plain, Plain, Enable)]
#[case::redb(Redb, Plain, Plain, Plain, Rekey)]
#[case::native_plain(Native, Plain, Compressed, CompressedEncrypted, Enable)]
#[case::native_encrypted(Native, Encrypted, CompressedEncrypted, Plain, Disable)]
#[case::native_compressed(Native, Compressed, Plain, Encrypted, Rekey)]
#[case::native_compressed_encrypted(Native, CompressedEncrypted, Encrypted, Compressed, Rekey)]
#[case::kv_plain(KeyValue, Plain, Compressed, CompressedEncrypted, Disable)]
#[case::kv_encrypted(KeyValue, Encrypted, CompressedEncrypted, Plain, Rekey)]
#[case::kv_compressed(KeyValue, Compressed, Plain, Encrypted, Enable)]
#[case::kv_compressed_encrypted(KeyValue, CompressedEncrypted, Encrypted, Compressed, Disable)]
fn public_restores_full_and_reprotected_incremental_images_after_source_and_ancestor_loss(
    #[case] provider: Provider,
    #[case] source_mode: FileMode,
    #[case] full_mode: FileMode,
    #[case] child_mode: FileMode,
    #[case] change: ProtectionChange,
) {
    let directory = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir_in(directory.path()).unwrap();
    let archive_dir = tempfile::tempdir_in(directory.path()).unwrap();
    let archive_path = archive_dir.path().join("repository");
    let copy_path = directory.path().join("portable");
    let control = StorageReadControl::with_limit(32 << 20);
    let limits = BackupLimits::new(4096, 4096).unwrap();
    let archive =
        LocalRepository::create(&archive_path, change.original(), limits, &control).unwrap();
    let source = provider.open(&source_dir.path().join("database"), source_mode);
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text NOT NULL); CREATE VIEW visible AS SELECT id, body FROM items; CREATE SEQUENCE ticket START 40 CACHE 1; INSERT INTO items VALUES(1,'first'),(2,'second')", &[]).unwrap();
    let full = capture(&source, &archive, None, &control);
    source
        .sql("INSERT INTO items VALUES(3,'third')", &[])
        .unwrap();
    assert_eq!(next_ticket(&source), Value::Int(40));
    let middle = capture(&source, &archive, Some(full.backup()), &control);
    source.sql("UPDATE items SET body='updated' WHERE id=2; DELETE FROM items WHERE id=1; INSERT INTO items VALUES(4,'fourth')", &[]).unwrap();
    assert_eq!(next_ticket(&source), Value::Int(41));
    let child = capture(&source, &archive, Some(middle.backup()), &control);
    let after_changes = Expected {
        rows: &[(2, "updated"), (3, "third"), (4, "fourth")],
        next_ticket: recovered_ticket(source_dir.path(), provider, source_mode),
    };
    assert_eq!(next_ticket(&source), Value::Int(42));
    drop(source);
    source_dir.close().unwrap();

    let full_path = directory.path().join("full");
    let authority = Engine::new();
    let mut cancelled = begin_restore(
        &authority,
        &archive,
        full.backup(),
        provider.destination(full_path.clone(), full_mode),
        RestoreOptions::new(WORKSPACE, 8 << 20),
        &control,
    )
    .unwrap();
    cancelled.advance(1024).unwrap();
    assert!(cancelled.cancel().unwrap().is_none());
    assert_eq!(cancelled.phase(), RestorePhase::Cancelled);
    drop((cancelled, authority));
    assert!(!full_path.exists());
    check_restored(
        &archive, full, &full_path, provider, full_mode, &ORIGINAL, &control,
    );
    for root in [middle, full] {
        archive.remove_backup(root.backup(), &control).unwrap();
        assert!(archive.open_backup(root.backup(), &control).is_err());
    }
    for pass in 1.. {
        let report = archive.collect_garbage(16, WORKSPACE, &control).unwrap();
        if report.remaining_files == 0 {
            break;
        }
        assert!(pass < 100, "ancestor cleanup did not finish");
    }
    let portable =
        LocalRepository::create(&copy_path, change.destination(), limits, &control).unwrap();
    let independent = standalone_copy(&archive, &portable, child, &control);
    drop((portable, archive));
    archive_dir.close().unwrap();
    assert!(LocalRepository::open(&copy_path, change.original(), limits, &control).is_err());
    if let Protection::Encrypted { domain, .. } = change.destination() {
        let previous_key = Protection::Encrypted {
            domain,
            key: BackupKey::new([11; 32]),
        };
        assert!(matches!(
            LocalRepository::open(&copy_path, previous_key, limits, &control),
            Err(BackupError::Integrity)
        ));
    }
    let portable =
        LocalRepository::open(&copy_path, change.destination(), limits, &control).unwrap();
    check_restored(
        &portable,
        independent,
        &directory.path().join("child"),
        provider,
        child_mode,
        &after_changes,
        &control,
    );
    assert_eq!(control.memory().used(), 0);
}
