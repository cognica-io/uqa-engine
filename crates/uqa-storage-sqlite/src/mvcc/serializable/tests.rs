//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent owners must preserve SSI dependencies, atomic updates and authoritative receipts.

use std::path::Path;

use uqa_storage::mvcc::{
    CommitStatus, PreparedRecordCommit, RecordWrite, SerializableKeySpace, SerializablePredicate,
    SerializableTransactionId, VersionedPersistence,
};

use super::*;
use crate::ManagedConnection;

mod liveness;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod process;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

fn open(path: &Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "serializable-transport-test"),
        2 => ManagedConnection::open_compressed(path, crate::SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "serializable-transport-test",
            crate::SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

fn point(key: &[u8]) -> SerializablePredicate<'_> {
    SerializablePredicate::point([7; 16], SerializableKeySpace::Rows, key)
}

fn admit_read(store: &SQLiteRecordStore, key: &[u8]) -> SerializableTransactionId {
    let control = control();
    let mut held = store.serializable_admission(&control).unwrap();
    let actor = held.graph_mut().admit(false, &control).unwrap();
    held.graph_mut()
        .observe_read(actor, point(key), &control)
        .unwrap();
    held.persist(&control).unwrap();
    actor
}

#[test]
fn independent_file_and_memory_owners_keep_write_skew_dependencies() {
    for mode in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared.db");
        let first = if mode == 4 {
            ManagedConnection::open_in_memory().unwrap()
        } else {
            open(&path, mode)
        };
        let second = if mode == 4 {
            first.new_session()
        } else {
            open(&path, mode)
        };
        let a = SQLiteRecordStore::new(&first).unwrap();
        let b = SQLiteRecordStore::new(&second).unwrap();
        let one = admit_read(&a, b"left");
        let two = admit_read(&b, b"right");
        assert_ne!(one, two);
        let control = control();
        let mut held = a.serializable_admission(&control).unwrap();
        held.graph_mut()
            .observe_write(one, point(b"right"), &control)
            .unwrap();
        held.graph_mut().prepare_commit(one, &control).unwrap();
        held.graph_mut().commit(one).unwrap();
        held.persist(&control).unwrap();
        let mut held = b.serializable_admission(&control).unwrap();
        assert!(matches!(
            held.graph_mut()
                .observe_write(two, point(b"left"), &control),
            Err(VersionError::SerializationConflict { .. })
        ));
        held.graph_mut().rollback(two).unwrap();
        held.graph_mut().reclaim();
        held.persist(&control).unwrap();
        drop((a, b));
        let reopened = SQLiteRecordStore::new(&first).unwrap();
        let next = admit_read(&reopened, b"later");
        assert_eq!(next.coordinator(), one.coordinator());
        assert!(next.allocation() > two.allocation());
    }
}

#[test]
fn a_discarded_or_cancelled_checkpoint_cannot_publish_half_an_observation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let actor = admit_read(&store, b"original");
    let control = control();
    for cancel in [false, true] {
        let mut held = store.serializable_admission(&control).unwrap();
        held.graph_mut().rollback(actor).unwrap();
        if cancel {
            control.cancellation().cancel();
            assert!(held.persist(&control).is_err());
            control.cancellation().reset();
        } else {
            drop(held);
        }
        store
            .serializable_admission(&control)
            .unwrap()
            .graph()
            .check_active(actor)
            .unwrap();
    }
    let held = store.serializable_admission(&control).unwrap();
    assert_eq!(
        held.connection
            .pragma_query_value(None, "busy_timeout", |row| row.get::<_, u32>(0))
            .unwrap(),
        0
    );
    drop(held);
    let auxiliary = store
        .connection
        .serializable_connection(store.identity, &control)
        .unwrap();
    assert!(auxiliary.lease_connection().unwrap().is_autocommit());
}

#[test]
fn a_new_owner_reconciles_committed_records_before_admitting_a_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipt.db");
    let control = control();
    let first = open(&path, 0);
    let store = SQLiteRecordStore::new(&first).unwrap();
    let actor = admit_read(&store, b"input");
    let transaction = store.allocate_transaction(&control).unwrap();
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"result",
            expected: None,
            value: Some(b"committed"),
        }],
        &control,
    )
    .unwrap();
    let mut held = store.serializable_admission(&control).unwrap();
    let publication = held
        .graph_mut()
        .prepare_publication(actor, transaction, prepared.fingerprint(), &control)
        .unwrap();
    held.persist(&control).unwrap();
    let held = store.serializable_admission(&control).unwrap();
    let receipt = store.commit(transaction, &prepared, &control).unwrap();
    // Simulate process loss after the durable record COMMIT and before retaining its SSI completion.
    drop((held, store, first));
    let second = open(&path, 0);
    let store = SQLiteRecordStore::new(&second).unwrap();
    let mut held = store.serializable_admission(&control).unwrap();
    held.graph_mut()
        .reconcile_publications(&control, |transaction| {
            store.commit_status(transaction, &control)
        })
        .unwrap();
    assert_eq!(
        held.graph_mut()
            .resolve_publication(publication, CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Committed(receipt)
    );
    let later = held.graph_mut().admit(true, &control).unwrap();
    let view = store.snapshot(&control).unwrap();
    assert!(view.get(b"result", &control).unwrap().is_some());
    held.persist(&control).unwrap();
    store
        .serializable_admission(&control)
        .unwrap()
        .graph()
        .check_active(later)
        .unwrap();
}

#[test]
fn encrypted_and_compressed_predicates_never_enter_plaintext_auxiliary_files() {
    const SECRET: &[u8] = b"confidential-logical-predicate-serializable-transport";
    for mode in [1, 3] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("protected.db");
        let connection = open(&path, mode);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let actor = admit_read(&store, SECRET);
        for entry in std::fs::read_dir(directory.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(!bytes.windows(SECRET.len()).any(|bytes| bytes == SECRET));
        }
        drop((store, connection));
        let reopened = open(&path, mode);
        SQLiteRecordStore::new(&reopened)
            .unwrap()
            .serializable_admission(&control())
            .unwrap()
            .graph()
            .check_active(actor)
            .unwrap();
    }
}

#[test]
fn malformed_auxiliary_state_is_rejected_without_recreation() {
    for sql in [
        "DROP TABLE _uqa_serializable_state",
        "PRAGMA user_version = 2",
        "CREATE TABLE sqliteextra (value TEXT)",
        "DELETE FROM _uqa_serializable_state",
        "UPDATE _uqa_serializable_state SET checkpoint = x'00'",
        "UPDATE _uqa_serializable_state SET database_id = zeroblob(16)",
        "ALTER TABLE _uqa_serializable_state ADD COLUMN unexpected INTEGER",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("malformed.db");
        let connection = open(&path, 0);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        admit_read(&store, b"retained");
        let auxiliary = store
            .connection
            .serializable_connection(store.identity, &control())
            .unwrap();
        auxiliary
            .lease_connection()
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        let path = auxiliary.database_path().unwrap();
        let before = std::fs::read(path).unwrap();
        assert!(store.serializable_admission(&control()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn exhausted_decode_allowance_preserves_the_previous_graph_and_releases_admission() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let key = vec![3; 20 * 1024];
    let actor = admit_read(&store, &key);
    let tiny = StorageReadControl::with_limit(1024);
    assert!(matches!(
        store.serializable_admission(&tiny),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    store
        .serializable_admission(&control())
        .unwrap()
        .graph()
        .check_active(actor)
        .unwrap();
}

#[test]
fn concurrent_first_admissions_share_one_incarnation_without_losing_participants() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("first-open.db");
        let first = open(&path, mode);
        let a = SQLiteRecordStore::new(&first).unwrap();
        let second = open(&path, mode);
        let b = SQLiteRecordStore::new(&second).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (one, two) = std::thread::scope(|scope| {
            let other = scope.spawn(|| {
                barrier.wait();
                admit_read(&b, b"right")
            });
            barrier.wait();
            let one = admit_read(&a, b"left");
            (one, other.join().unwrap())
        });
        assert_eq!(one.coordinator(), two.coordinator());
        let mut allocations = [one.allocation(), two.allocation()];
        allocations.sort_unstable();
        assert_eq!(allocations, [1, 2]);
        let held = a.serializable_admission(&control()).unwrap();
        held.graph().check_active(one).unwrap();
        held.graph().check_active(two).unwrap();
    }
}

#[test]
fn abandoning_initial_creation_leaves_the_next_owner_able_to_initialize() {
    for mode in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("initial-rollback.db");
        let connection = if mode == 4 {
            ManagedConnection::open_in_memory().unwrap()
        } else {
            open(&path, mode)
        };
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
        let mut held = store.serializable_admission(&control).unwrap();
        let discarded = held.graph_mut().admit(true, &control).unwrap();
        drop(held);
        let committed = admit_read(&store, b"retained");
        assert_ne!(discarded.coordinator(), committed.coordinator());
        assert_eq!(committed.allocation(), 1);
    }
}
