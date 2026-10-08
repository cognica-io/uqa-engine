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

/// What another connection holds while the operation first asks for the database.
#[derive(Clone, Copy)]
enum Holder {
    /// A read transaction, which a commit under a rollback journal waits for.
    Reader,
    /// A write transaction, which another writer waits for.
    Writer,
    /// The whole database, which under a rollback journal a reader waits for as well.
    Exclusive,
}

fn after_physical_contention<T: Send>(
    path: &std::path::Path,
    store: &SQLiteRecordStore,
    control: &StorageReadControl,
    holder: Holder,
    cancel: bool,
    operation: impl FnOnce(&Connection) -> T + Send,
) -> T {
    let begin = match holder {
        Holder::Reader => "BEGIN; SELECT allocated FROM _uqa_mvcc_metadata",
        Holder::Writer => "BEGIN IMMEDIATE",
        Holder::Exclusive => "BEGIN EXCLUSIVE",
    };
    let holder = Connection::open(path).unwrap();
    holder.execute_batch(begin).unwrap();
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
    let id = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Writer,
        false,
        |connection| write::allocate(connection, store.identity, None, &control),
    )
    .unwrap();
    assert_eq!(id.allocation(), 1);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    let allocation = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Writer,
        false,
        |connection| {
            super::super::identifiers::allocate(
                connection,
                store.identity,
                None,
                b"identities",
                uqa_storage::mvcc::IdentifierRequest::Observe(41),
                &control,
            )
        },
    )
    .unwrap();
    assert_eq!(allocation.watermark(), 41);
    assert_eq!(
        store.identifier_watermark(b"identities", &control).unwrap(),
        Some(41)
    );
    let prepared = prepared(b"item", b"published", &control);
    let receipt = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Writer,
        false,
        |connection| write::commit(connection, id, &prepared, None, &control, |_| {}),
    )
    .unwrap();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    // Recording an abort is admitted the same way, so a conflicting writer cannot turn rollback into a deferred cleanup failure.
    let aborted = store.allocate_transaction(&control).unwrap();
    let status = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Writer,
        false,
        |connection| write::abort(connection, aborted, None, &control),
    )
    .unwrap();
    assert_eq!(status, CommitStatus::Aborted);
    assert_eq!(
        store.commit_status(aborted, &control).unwrap(),
        CommitStatus::Aborted
    );
}

#[test]
fn cancelling_writer_admission_does_not_consume_a_transaction_or_identifier() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancel-admission.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let result = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Writer,
        true,
        |connection| write::allocate(connection, store.identity, None, &control),
    );
    assert!(matches!(
        result,
        Err(Error::Version(VersionError::Cancelled(_)))
    ));
    control.cancellation().reset();
    assert_eq!(
        store.allocate_transaction(&control).unwrap().allocation(),
        1
    );
    let result = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Writer,
        true,
        |connection| {
            super::super::identifiers::allocate(
                connection,
                store.identity,
                None,
                b"identities",
                uqa_storage::mvcc::IdentifierRequest::Observe(41),
                &control,
            )
        },
    );
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
    let connection = ManagedConnection::open_auxiliary(&path, None).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    store
        .with(|connection| {
            let mode: String =
                connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            assert_eq!(mode, "delete", "readers must block physical COMMIT");
            connection.execute_batch("CREATE TRIGGER observe_versions AFTER INSERT ON _uqa_mvcc_versions BEGIN SELECT observe_version(); END")?;
            Ok(())
        })
        .unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let prepared = prepared(b"item", b"published", &control);
    let staged = Arc::new(AtomicUsize::new(0));
    let result = after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Reader,
        cancel,
        |connection| {
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
            write::commit(connection, id, &prepared, None, &control, |_| {})
        },
    );
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
fn an_acknowledgement_waits_for_a_writer_that_holds_a_rollback_journal_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("acknowledged.db");
    // Under a rollback journal a reader waits for a writer that holds the database, as the readers of a compressed database and of the notification state file do.
    let connection = ManagedConnection::open_auxiliary(&path, None).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store
        .commit(id, &prepared(b"item", b"published", &control), &control)
        .unwrap();
    // The acknowledgement reads its owner kind before its transaction begins. That read meets the busy database first and is waited for like the transaction, instead of failing a commit that has already been written.
    after_physical_contention(
        &path,
        &store,
        &control,
        Holder::Exclusive,
        false,
        |connection| {
            super::super::receipts::acknowledge(
                connection,
                None,
                uqa_storage::mvcc::ReceiptAcknowledgement::Committed(receipt),
                &control,
            )
        },
    )
    .unwrap();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn a_reader_blocking_commit_does_not_replay_staged_records() {
    commit_behind_reader(false);
}

#[test]
fn retained_completion_reads_retry_busy_and_preserve_cancelled_owners() {
    for cancel in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retained.db");
        let connection = ManagedConnection::open_auxiliary(&path, None).unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
        let owner = store.allocate_managed_transaction(&control).unwrap();
        let receipt = store
            .commit(
                owner.transaction(),
                &prepared(b"item", b"published", &control),
                &control,
            )
            .unwrap();
        let acknowledgement = uqa_storage::mvcc::ReceiptAcknowledgement::Committed(receipt);
        let result = after_physical_contention(
            &path,
            &store,
            &control,
            Holder::Exclusive,
            cancel,
            |connection| {
                super::super::receipts::validate_retained(
                    connection,
                    None,
                    acknowledgement,
                    &control,
                )
            },
        );
        if cancel {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap());
        }
        let recovery = super::control();
        assert_eq!(store.reclaim_transaction_receipts(&recovery).unwrap(), 0);
        assert_eq!(
            store.commit_status(owner.transaction(), &recovery).unwrap(),
            CommitStatus::Committed(receipt)
        );
        store
            .acknowledge_retained_transaction(&owner, acknowledgement, &recovery)
            .unwrap();
        drop(owner);
        assert_eq!(store.reclaim_transaction_receipts(&recovery).unwrap(), 1);
    }
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
