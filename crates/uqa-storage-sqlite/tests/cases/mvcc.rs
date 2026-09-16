//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical `SQLite` records exercise the common transaction contract across supported native open modes.

use std::{
    path::Path,
    sync::{mpsc, Arc},
    time::Duration,
};

use uqa_storage::mvcc::*;
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::KeyValueStore;
use uqa_storage_sqlite::{ManagedConnection, SQLiteCompressionOptions, SQLiteRecordStore};

#[path = "mvcc/key_value.rs"]
mod key_value;
#[path = "mvcc/native_btree.rs"]
mod native_btree;
#[path = "mvcc/native_catalog.rs"]
mod native_catalog;
#[path = "mvcc/native_columns.rs"]
mod native_columns;
#[path = "mvcc/native_documents.rs"]
mod native_documents;
#[path = "mvcc/native_relations.rs"]
mod native_relations;
#[path = "mvcc/native_sequences.rs"]
mod native_sequences;
#[path = "mvcc/native_tables.rs"]
mod native_tables;

#[derive(Clone, Copy, Debug)]
enum Mode {
    Plain,
    Encrypted,
    Compressed,
    CompressedEncrypted,
}
const MODES: [Mode; 4] = [
    Mode::Plain,
    Mode::Encrypted,
    Mode::Compressed,
    Mode::CompressedEncrypted,
];

fn open(mode: Mode, path: &Path) -> ManagedConnection {
    match mode {
        Mode::Plain => ManagedConnection::open(path),
        Mode::Encrypted => ManagedConnection::open_encrypted(path, "MVCC fixture key"),
        Mode::Compressed => {
            ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default())
        }
        Mode::CompressedEncrypted => ManagedConnection::open_compressed_encrypted(
            path,
            "MVCC fixture key",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 24)
}
fn session(store: &SQLiteRecordStore) -> VersionedKeyValueStore {
    VersionedKeyValueStore::new(
        Arc::new(store.clone()),
        None,
        VersionedSessionOptions::default(),
    )
}
fn batch(writes: &[RecordWrite<'_>]) -> PreparedRecordCommit {
    PreparedRecordCommit::new(writes, &control()).unwrap()
}
fn write<'a>(
    key: &'a [u8],
    expected: Option<CommitSequence>,
    value: Option<&'a [u8]>,
) -> RecordWrite<'a> {
    RecordWrite {
        key,
        expected,
        value,
    }
}

#[test]
fn independent_database_owners_commit_while_another_session_retains_private_changes() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("records.db");
            {
                let store = SQLiteRecordStore::new(&open(mode, &path)).unwrap();
                let a = session(&store);
                a.begin_transaction().unwrap();
                a.put(b"a", b"first").unwrap();
                a.savepoint("keep").unwrap();
                a.put(b"a", b"second").unwrap();
                let other_path = path.clone();
                let (sent, received) = mpsc::channel();
                let worker = std::thread::spawn(move || {
                    let store = SQLiteRecordStore::new(&open(mode, &other_path)).unwrap();
                    let b = session(&store);
                    b.begin_transaction().unwrap();
                    b.put(b"b", b"independent").unwrap();
                    b.commit_transaction().unwrap();
                    sent.send(()).unwrap();
                });
                let completed = received.recv_timeout(Duration::from_secs(20));
                if completed.is_err() {
                    a.rollback_transaction().unwrap();
                    worker.join().unwrap();
                    panic!("independent owner did not complete for {mode:?}: {completed:?}");
                }
                worker.join().unwrap();
                assert!(a.in_transaction());
                assert_eq!(a.get(b"b").unwrap(), None);
                assert_eq!(session(&store).get(b"b").unwrap().unwrap(), b"independent");
                match ending {
                    "commit" => a.commit_transaction().unwrap(),
                    "rollback" => a.rollback_transaction().unwrap(),
                    _ => {
                        a.rollback_to_savepoint("keep").unwrap();
                        a.commit_transaction().unwrap();
                    }
                }
            }
            let store = SQLiteRecordStore::new(&open(mode, &path)).unwrap();
            let current = session(&store);
            assert_eq!(current.get(b"b").unwrap().unwrap(), b"independent");
            let expected = match ending {
                "commit" => Some(b"second".as_slice()),
                "savepoint" => Some(b"first".as_slice()),
                _ => None,
            };
            assert_eq!(current.get(b"a").unwrap().as_deref(), expected);
        }
    }
}

#[test]
fn commit_receipts_survive_reopen_and_make_retries_idempotent_in_every_file_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts.db");
        let control = control();
        let prepared = batch(&[
            write(b"a", None, Some(b"one")),
            write(b"z", None, Some(b"two")),
        ]);
        let (id, receipt) = {
            let store = SQLiteRecordStore::new(&open(mode, &path)).unwrap();
            let id = store.allocate_transaction(&control).unwrap();
            let receipt = store.commit(id, &prepared, &control).unwrap();
            assert_eq!(
                store.abort(id, &control).unwrap(),
                CommitStatus::Committed(receipt)
            );
            (id, receipt)
        };
        let store = SQLiteRecordStore::new(&open(mode, &path)).unwrap();
        assert_eq!(
            store.commit_status(id, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(store.commit(id, &prepared, &control).unwrap(), receipt);
        assert_eq!(session(&store).get(b"a").unwrap().unwrap(), b"one");
        assert!(matches!(
            store.commit(id, &batch(&[write(b"a", None, Some(b"changed"))]), &control),
            Err(CommitFailure::Rejected(VersionError::CommitMismatch))
        ));
        let next = store.allocate_transaction(&control).unwrap();
        assert!(next.allocation() > id.allocation());
        assert_eq!(store.abort(next, &control).unwrap(), CommitStatus::Aborted);
        assert_eq!(
            store.snapshot(&control).unwrap().sequence(),
            receipt.sequence
        );
    }
}

#[test]
fn conflicts_validate_every_record_before_publishing_any_change() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let a = session(&store);
    let b = session(&store);
    a.put(b"z", b"initial").unwrap();
    a.begin_transaction().unwrap();
    a.put(b"a", b"must remain private").unwrap();
    a.put(b"z", b"stale").unwrap();
    b.put(b"z", b"winner").unwrap();
    assert!(a.commit_transaction().is_err());
    let id = a.pending_commit().unwrap();
    assert_eq!(
        store.commit_status(id, &control()).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(b.get(b"a").unwrap(), None);
    a.rollback_transaction().unwrap();
    assert_eq!(
        store.commit_status(id, &control()).unwrap(),
        CommitStatus::Aborted
    );
    assert_eq!(a.get(b"z").unwrap().unwrap(), b"winner");
}

#[test]
fn historical_binary_scans_preserve_tombstones_and_both_prefix_boundaries() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    let keys = [
        b"".as_slice(),
        b"a",
        b"a\0",
        b"a\xff",
        b"b",
        b"\xff",
        b"\xff\0",
        b"\xff\xff",
    ];
    let writes: Vec<_> = keys.iter().map(|key| write(key, None, Some(key))).collect();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &batch(&writes), &control).unwrap();
    let old = store.snapshot(&control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(
            id,
            &batch(&[
                write(b"a", Some(receipt.sequence), None),
                write(b"aa", None, Some(b"future")),
            ]),
            &control,
        )
        .unwrap();
    for prefix in [b"".as_slice(), b"a", b"\xff", b"absent"] {
        for after in [
            None,
            Some(b"".as_slice()),
            Some(b"a"),
            Some(b"a\0"),
            Some(b"z"),
            Some(b"\xff\xff"),
        ] {
            for limit in [0, 1, 2, 20] {
                let expected: Vec<_> = keys
                    .iter()
                    .filter(|key| {
                        key.starts_with(prefix) && after.is_none_or(|after| **key > after)
                    })
                    .take(limit)
                    .map(|key| key.to_vec())
                    .collect();
                let page = old.scan(prefix, after, limit, &control).unwrap();
                assert_eq!(
                    page.iter()
                        .map(|entry| entry.key.to_vec())
                        .collect::<Vec<_>>(),
                    expected
                );
                for entry in page.iter() {
                    assert_eq!(&***entry.version.value().unwrap(), &*entry.key);
                }
            }
        }
    }
    let current = store.snapshot(&control).unwrap();
    assert!(current
        .get(b"a", &control)
        .unwrap()
        .unwrap()
        .value()
        .is_none());
    assert!(old.get(b"aa", &control).unwrap().is_none());
    assert_eq!(
        &***current
            .get(b"aa", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"future"
    );
}

#[test]
fn controlled_reads_and_cancelled_commits_preserve_outcomes_and_release_allowances() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let large = vec![3; 65536];
    let prepared = batch(&[write(b"large", None, Some(&large))]);
    control.cancellation().cancel();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::Cancelled(_)))
    ));
    control.cancellation().reset();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    store.commit(id, &prepared, &control).unwrap();
    assert!(matches!(
        store.snapshot(&StorageReadControl::with_limit(0)),
        Err(VersionError::Memory(_))
    ));
    let snapshot = store.snapshot(&control).unwrap();
    let tiny = StorageReadControl::with_limit(1024);
    assert!(matches!(
        snapshot.get(b"large", &tiny),
        Err(VersionError::Memory(_))
    ));
    assert!(matches!(
        snapshot.scan(b"", None, 1, &tiny),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    let mut calls = 0;
    let error = snapshot
        .visit_prefix(b"", None, 10, &control, &mut |_, _| {
            calls += 1;
            control.cancellation().cancel();
            Ok(true)
        })
        .unwrap_err();
    assert!(matches!(error, VersionError::Cancelled(_)));
    assert_eq!(calls, 1);
    control.cancellation().reset();
    assert!(snapshot.get(b"large", &control).unwrap().is_some());
}

#[test]
fn a_busy_native_commit_is_retained_as_uncertain_and_the_same_batch_can_resolve() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("busy.db");
    let connection = open(Mode::Compressed, &path);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let prepared = batch(&[
        write(b"a", None, Some(b"one")),
        write(b"b", None, Some(b"two")),
    ]);
    connection
        .with(|connection| {
            connection.busy_timeout(Duration::ZERO)?;
            Ok(())
        })
        .unwrap();
    let reader = open(Mode::Compressed, &path);
    reader.begin_deferred_transaction().unwrap();
    reader.pin_transaction_snapshot().unwrap();
    assert!(
        matches!(store.commit(id, &prepared, &control), Err(CommitFailure::Indeterminate { transaction, .. }) if transaction == id)
    );
    reader.rollback_transaction().unwrap();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    assert!(store
        .snapshot(&control)
        .unwrap()
        .get(b"a", &control)
        .unwrap()
        .is_none());
    let receipt = store.commit(id, &prepared, &control).unwrap();
    assert_eq!(store.commit(id, &prepared, &control).unwrap(), receipt);
    assert_eq!(session(&store).get(b"b").unwrap().unwrap(), b"two");
}

#[test]
fn foreign_allocations_and_unknown_outcomes_are_not_treated_as_rollback() {
    let a = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let b = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    let id = a.allocate_transaction(&control).unwrap();
    assert!(matches!(
        b.abort(id, &control),
        Err(VersionError::WrongDatabase)
    ));
    assert!(matches!(
        b.commit(id, &batch(&[]), &control),
        Err(CommitFailure::Rejected(VersionError::WrongDatabase))
    ));
    let unknown = StorageTransactionId::new(a.database_id(), id.allocation() + 1).unwrap();
    assert_eq!(a.abort(unknown, &control).unwrap(), CommitStatus::Unknown);
    assert!(matches!(
        a.commit(unknown, &batch(&[]), &control),
        Err(CommitFailure::Rejected(VersionError::UnknownTransaction))
    ));
    let receipt = a.commit(id, &batch(&[]), &control).unwrap();
    assert_eq!(receipt.sequence, CommitSequence::INITIAL);
}

#[cfg(not(target_os = "emscripten"))]
#[test]
fn another_process_publishes_records_before_the_parent_transaction_ends() {
    process_writer_schedule(false);
}

#[cfg(not(target_os = "emscripten"))]
#[test]
fn another_process_uses_the_public_key_value_store_before_the_parent_transaction_ends() {
    process_writer_schedule(true);
}

#[cfg(not(target_os = "emscripten"))]
fn process_writer_schedule(public_store: bool) {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    for (mode_index, mode) in MODES.into_iter().enumerate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("process.db");
        let a = process_store(mode, &path, public_store);
        a.begin_transaction().unwrap();
        a.put(b"parent", b"private").unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mvcc::process_writer_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("UQA_SQLITE_RECORD_TEST_FILE", &path)
            .env("UQA_SQLITE_RECORD_TEST_MODE", mode_index.to_string())
            .env("UQA_SQLITE_RECORD_TEST_PUBLIC", public_store.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "child writer could not complete for {mode:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("record-commit-complete"));
        assert!(a.in_transaction());
        assert_eq!(a.get(b"child").unwrap(), None);
        assert_eq!(
            a.open_session().unwrap().get(b"child").unwrap().unwrap(),
            b"committed"
        );
        a.commit_transaction().unwrap();
        assert_eq!(a.get(b"child").unwrap().unwrap(), b"committed");
        assert_eq!(a.get(b"parent").unwrap().unwrap(), b"private");
    }
}

#[cfg(not(target_os = "emscripten"))]
fn process_store(mode: Mode, path: &Path, public_store: bool) -> Box<dyn KeyValueStore> {
    let connection = open(mode, path);
    if public_store {
        Box::new(uqa_storage_sqlite::SQLiteKeyValueStore::new(connection).unwrap())
    } else {
        Box::new(session(&SQLiteRecordStore::new(&connection).unwrap()))
    }
}

#[cfg(not(target_os = "emscripten"))]
#[test]
#[ignore = "invoked by the process-isolation parent with an owned fixture path"]
fn process_writer_helper() {
    let path = std::env::var_os("UQA_SQLITE_RECORD_TEST_FILE").expect("owned fixture path");
    let mode = MODES[std::env::var("UQA_SQLITE_RECORD_TEST_MODE")
        .unwrap()
        .parse::<usize>()
        .unwrap()];
    let public = std::env::var("UQA_SQLITE_RECORD_TEST_PUBLIC").unwrap() == "true";
    process_store(mode, Path::new(&path), public)
        .put(b"child", b"committed")
        .unwrap();
    println!("record-commit-complete");
}
