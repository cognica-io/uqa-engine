//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical contention must preserve admission, cancellation and exact publication.

use super::*;
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};

type BusyGate = (mpsc::Sender<()>, mpsc::Receiver<()>);

thread_local! {
    static BUSY_GATE: RefCell<Option<BusyGate>> = const { RefCell::new(None) };
}

fn reject_first_busy(_attempt: i32) -> bool {
    BUSY_GATE.with(|gate| {
        if let Some((entered, release)) = gate.borrow_mut().take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(30)).unwrap();
        }
    });
    // SQLite must return BUSY even though the conflicting holder has now released.
    false
}

fn after_physical_contention<T: Send>(
    path: &std::path::Path,
    store: &SQLiteRecordStore,
    control: &StorageReadControl,
    read_only_holder: bool,
    cancel: bool,
    operation: impl FnOnce(&Connection) -> T + Send,
) -> T {
    let holder = Connection::open(path).unwrap();
    holder
        .execute_batch(if read_only_holder {
            "BEGIN; SELECT allocated FROM _uqa_mvcc_metadata"
        } else {
            "BEGIN IMMEDIATE"
        })
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            BUSY_GATE.with(|gate| *gate.borrow_mut() = Some((entered_tx, release_rx)));
            let output = store.with(|connection| {
                connection.busy_handler(Some(reject_first_busy))?;
                let result = operation(connection);
                connection.busy_timeout(Duration::from_secs(5))?;
                Ok(result)
            });
            BUSY_GATE.with(|gate| gate.borrow_mut().take());
            output.unwrap()
        });
        let entered = entered_rx.recv_timeout(Duration::from_secs(30));
        if cancel {
            control.cancellation().cancel();
        }
        // Release before assertions and joining, even when admission fails to reach the gate.
        holder.execute_batch("ROLLBACK").unwrap();
        let _ = release_tx.send(());
        let result = worker.join().unwrap();
        entered.expect("the operation never reached physical contention");
        result
    })
}

#[test]
fn transaction_and_identifier_admission_survive_a_rejected_busy_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("writer-admission.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let id = after_physical_contention(&path, &store, &control, false, false, |connection| {
        write::allocate(connection, store.identity, false, &control)
    })
    .unwrap();
    assert_eq!(id.allocation(), 1);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    let allocation =
        after_physical_contention(&path, &store, &control, false, false, |connection| {
            super::super::identifiers::allocate(
                connection,
                store.identity,
                false,
                b"identities",
                uqa_storage::mvcc::IdentifierRequest::Observe(41),
                &control,
            )
        })
        .unwrap();
    assert_eq!(allocation.watermark(), 41);
    assert_eq!(
        store.identifier_watermark(b"identities", &control).unwrap(),
        Some(41)
    );
    let prepared = prepared(b"item", b"published", &control);
    let receipt = after_physical_contention(&path, &store, &control, false, false, |connection| {
        write::commit(connection, id, &prepared, false, &control)
    })
    .unwrap();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
}

#[test]
fn cancelling_writer_admission_does_not_consume_a_transaction_or_identifier() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancel-admission.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let result = after_physical_contention(&path, &store, &control, false, true, |connection| {
        write::allocate(connection, store.identity, false, &control)
    });
    assert!(matches!(
        result,
        Err(Error::Version(VersionError::Cancelled(_)))
    ));
    control.cancellation().reset();
    assert_eq!(
        store.allocate_transaction(&control).unwrap().allocation(),
        1
    );
    let result = after_physical_contention(&path, &store, &control, false, true, |connection| {
        super::super::identifiers::allocate(
            connection,
            store.identity,
            false,
            b"identities",
            uqa_storage::mvcc::IdentifierRequest::Observe(41),
            &control,
        )
    });
    assert!(matches!(
        result,
        Err(Error::Version(VersionError::Cancelled(_)))
    ));
    control.cancellation().reset();
    assert_eq!(
        store.identifier_watermark(b"identities", &control).unwrap(),
        None
    );
}

fn commit_behind_reader(cancel: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader-at-commit.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    store
        .with(|connection| {
            connection.pragma_update(None, "journal_mode", "DELETE")?;
            connection.execute_batch("CREATE TRIGGER observe_versions AFTER INSERT ON _uqa_mvcc_versions BEGIN SELECT observe_version(); END")?;
            Ok(())
        })
        .unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let prepared = prepared(b"item", b"published", &control);
    let staged = Arc::new(AtomicUsize::new(0));
    let result = after_physical_contention(&path, &store, &control, true, cancel, |connection| {
        let staged = Arc::clone(&staged);
        connection
            .create_scalar_function(
                "observe_version",
                0,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |_| {
                    staged.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                },
            )
            .unwrap();
        write::commit(connection, id, &prepared, false, &control)
    });
    assert_eq!(staged.load(Ordering::SeqCst), 1, "publication was restaged");
    control.cancellation().reset();
    let snapshot = store.snapshot(&control).unwrap();
    if cancel {
        assert!(matches!(
            result,
            Err(CommitFailure::Rejected(VersionError::Cancelled(_)))
        ));
        assert_eq!(
            store.commit_status(id, &control).unwrap(),
            CommitStatus::Pending
        );
        assert!(snapshot.get(b"item", &control).unwrap().is_none());
    } else {
        let receipt = result.unwrap();
        assert_eq!(
            store.commit_status(id, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(snapshot.sequence(), receipt.sequence);
        assert_eq!(
            &***snapshot
                .get(b"item", &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            b"published"
        );
    }
    store
        .with(|connection| {
            assert!(connection.is_autocommit());
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_reader_blocking_commit_does_not_replay_staged_records() {
    commit_behind_reader(false);
}

#[test]
fn cancellation_of_a_busy_commit_preserves_pending_receipt_and_rolls_back_records() {
    commit_behind_reader(true);
}

#[test]
fn record_write_scope_restores_pooled_timeout_after_error_and_unwind() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    for outcome in 0..3 {
        store
            .with(|connection| {
                connection.busy_timeout(Duration::from_millis(37))?;
                Ok(())
            })
            .unwrap();
        let mut calls = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.with_write(&control, |connection| {
                calls += 1;
                assert_eq!(
                    connection
                        .pragma_query_value(None, "busy_timeout", |row| row.get::<_, u32>(0))?,
                    0
                );
                match outcome {
                    0 => Ok(()),
                    1 => Err(rusqlite::Error::InvalidQuery.into()),
                    _ => panic!("injected record write unwind"),
                }
            })
        }));
        assert_eq!(calls, 1, "a non-BUSY operation was replayed");
        match outcome {
            0 => result.unwrap().unwrap(),
            1 => assert!(result.unwrap().is_err()),
            _ => assert!(result.is_err()),
        }
        store
            .with(|connection| {
                assert!(connection.is_autocommit());
                assert_eq!(
                    connection
                        .pragma_query_value(None, "busy_timeout", |row| row.get::<_, u32>(0))?,
                    37
                );
                Ok(())
            })
            .unwrap();
    }
}
