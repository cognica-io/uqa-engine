//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public SQL admission uses the same participant and fixed view on all concurrent providers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use uqa_core::Value;
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};
use uqa_storage::mvcc::{
    SafeSnapshot, SerializableTransactionId, VersionError, VersionedPersistence,
};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage_sqlite::SQLiteRecordStore;

pub(super) fn participant(session: &Session) -> Option<SerializableTransactionId> {
    session
        .backend
        .serializable_session()
        .unwrap()
        .serializable_read_context()
        .unwrap()
        .map(|context| context.id())
}

pub(super) fn pending_snapshot(
    records: &dyn VersionedPersistence,
    id: SerializableTransactionId,
    cancellation: &uqa_core::CancellationToken,
) {
    let control = StorageReadControl::with_limit(16 << 20);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut status = None;
        let result = records
            .serializable_coordinator()
            .unwrap()
            .with_serializable_admission(&control, &mut |graph, _| {
                match graph.safe_snapshot(id, &control) {
                    Ok(current) => status = Some(current),
                    Err(VersionError::UnknownTransaction) => {}
                    Err(error) => return Err(error),
                }
                Ok(())
            });
        if result.is_err()
            || Instant::now() >= deadline
            || matches!(status, Some(SafeSnapshot::Safe | SafeSnapshot::Unsafe))
        {
            cancellation.cancel();
        }
        result.unwrap();
        assert!(
            Instant::now() < deadline,
            "reader did not enter safe-snapshot waiting"
        );
        assert!(
            !matches!(status, Some(SafeSnapshot::Safe | SafeSnapshot::Unsafe)),
            "unexpected candidate state: {status:?}"
        );
        if status == Some(SafeSnapshot::Pending) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn observed_fixture(
    provider: usize,
    path: &std::path::Path,
) -> (Session, Arc<dyn VersionedPersistence>) {
    let (pair, records): (_, Arc<dyn VersionedPersistence>) = match provider {
        0 => {
            let connection = ManagedConnection::open_in_memory().unwrap();
            let provider = SQLiteStorageProvider::new(connection.clone());
            let pair = provider.open_session().unwrap();
            let records = SQLiteRecordStore::for_native(
                &connection,
                &StorageReadControl::with_limit(16 << 20),
            )
            .unwrap();
            (pair, Arc::new(records))
        }
        1 => {
            let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
            let records = SQLiteRecordStore::new(&provider.store().connection()).unwrap();
            (provider.open_session().unwrap(), Arc::new(records))
        }
        _ => {
            let provider = RedbStorage::open(path).unwrap();
            let records = provider.record_store().unwrap();
            (provider.open_session().unwrap(), Arc::new(records))
        }
    };
    let session = Session::new(pair);
    session.sql("CREATE TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER); CREATE TABLE right_t (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO left_t VALUES (1, 1); INSERT INTO right_t VALUES (1, 1)");
    (session, records)
}

#[test]
fn first_query_admits_once_and_keeps_the_selected_view_after_refresh_and_savepoint_undo() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        let b = a.sibling();
        a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SET TRANSACTION READ WRITE; SHOW transaction_isolation; SAVEPOINT before_read");
        assert!(participant(&a).is_none());
        b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
        assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(2));
        let original = participant(&a).unwrap();
        b.sql("UPDATE left_t SET v = 3 WHERE id = 1");
        a.sql("ROLLBACK TO before_read");
        assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(2));
        assert_eq!(participant(&a), Some(original));
        assert_eq!(
            a.engine
                .sql("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", &[])
                .unwrap_err()
                .sqlstate(),
            Some("25001")
        );
        a.sql("ROLLBACK");
        assert!(participant(&a).is_none());
    }
}

#[test]
fn public_serializable_write_skew_rejects_either_second_committer() {
    for reverse in [false, true] {
        let (_directory, fixtures) = fixtures();
        for a in fixtures {
            let b = a.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            a.sql("SELECT v FROM left_t");
            b.sql("SELECT v FROM right_t");
            a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            let (winner, loser) = if reverse { (&b, &a) } else { (&a, &b) };
            winner.sql("COMMIT");
            assert_eq!(
                loser.engine.sql("COMMIT", &[]).unwrap_err().sqlstate(),
                Some("40001")
            );
            assert_eq!(loser.engine.transaction_depth(), 0);
        }
    }
}

#[test]
fn independent_serializable_writes_commit_while_a_peer_transaction_stays_open() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        let b = a.sibling();
        a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; INSERT INTO left_t VALUES (2, 2)");
        b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; INSERT INTO right_t VALUES (2, 2); COMMIT");
        assert_eq!(a.engine.transaction_depth(), 1);
        a.sql("COMMIT");
        assert_eq!(
            a.sql("SELECT COUNT(*) AS n FROM left_t").rows[0]["n"],
            Value::Int(2)
        );
        assert_eq!(
            a.sql("SELECT COUNT(*) AS n FROM right_t").rows[0]["n"],
            Value::Int(2)
        );
    }
}

#[test]
fn implicit_sql_and_direct_mutations_admit_through_session_defaults() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        let backend = Arc::clone(&a.backend);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&calls);
        a.engine
            .register_scalar_function_with_options(
                "admission_probe",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |_: &[Value]| {
                    assert!(backend
                        .serializable_session()
                        .unwrap()
                        .serializable_read_context()
                        .unwrap()
                        .is_some());
                    observed_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Value::Int(7))
                },
            )
            .unwrap();
        a.sql("SET default_transaction_isolation = 'serializable'");
        assert_eq!(
            a.sql("SELECT admission_probe() AS n").rows[0]["n"],
            Value::Int(7)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(participant(&a).is_none());
        a.engine
            .sql_cursor("SELECT admission_probe() AS n", &[])
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(participant(&a).is_none());
        a.sql("PREPARE admission_statement AS SELECT admission_probe() AS n");
        assert_eq!(
            a.sql("EXECUTE admission_statement").rows[0]["n"],
            Value::Int(7)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(participant(&a).is_none());
        a.engine
            .transaction(|engine| {
                assert!(participant(&a).is_none());
                engine.add_document(
                    "left_t",
                    3,
                    [("id".into(), Value::Int(3)), ("v".into(), Value::Int(3))]
                        .into_iter()
                        .collect(),
                )?;
                assert!(participant(&a).is_some());
                Ok(())
            })
            .unwrap();
        assert!(participant(&a).is_none());
        assert_eq!(
            a.sql("SELECT v FROM left_t WHERE id = 3").rows[0]["v"],
            Value::Int(3)
        );
    }
}

#[test]
fn read_only_deferrable_waits_without_blocking_writer_commit_and_reselects_only_unsafe_views() {
    for unsafe_candidate in [false, true] {
        for provider in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let (writer, records) =
                observed_fixture(provider, &directory.path().join("admission.redb"));
            let reader = writer.sibling();
            writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
            let mut previous = participant(&writer).unwrap();
            if unsafe_candidate {
                let first = writer.sibling();
                first.sql(
                    "BEGIN ISOLATION LEVEL SERIALIZABLE; UPDATE left_t SET v = 2 WHERE id = 1",
                );
                previous = participant(&first).unwrap();
                first.sql("COMMIT");
            }
            let candidate = SerializableTransactionId::new(
                previous.database(),
                previous.coordinator(),
                previous.allocation() + 1,
            )
            .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed_calls = Arc::clone(&calls);
            reader
                .engine
                .register_scalar_function_with_options(
                    "admission_probe",
                    SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                    move |_: &[Value]| {
                        observed_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(Value::Int(1))
                    },
                )
                .unwrap();
            reader.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE");
            let cancellation = reader.engine.cancellation_token();
            std::thread::scope(|scope| {
                let query = scope.spawn(|| {
                    reader
                        .engine
                        .sql("SELECT v, admission_probe() FROM right_t", &[])
                });
                pending_snapshot(records.as_ref(), candidate, &cancellation);
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                let publication = writer
                    .engine
                    .sql("UPDATE right_t SET v = 2 WHERE id = 1; COMMIT", &[]);
                if publication.is_err() {
                    cancellation.cancel();
                }
                publication.unwrap();
                let result = query.join().unwrap().unwrap();
                assert_eq!(
                    result.rows[0]["v"],
                    Value::Int(if unsafe_candidate { 2 } else { 1 })
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    participant(&reader).unwrap() == candidate,
                    !unsafe_candidate
                );
                reader.sql("COMMIT");
            });
        }
    }
}

#[test]
fn safe_snapshot_keeps_old_rows_with_catalogs_committed_during_admission_wait() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let (writer, records) = observed_fixture(provider, &directory.path().join("catalog.redb"));
        let reader = writer.sibling();
        writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
        let previous = participant(&writer).unwrap();
        let candidate = SerializableTransactionId::new(
            previous.database(),
            previous.coordinator(),
            previous.allocation() + 1,
        )
        .unwrap();
        reader.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE");
        let cancellation = reader.engine.cancellation_token();
        std::thread::scope(|scope| {
            let query = scope.spawn(|| {
                reader
                    .engine
                    .sql("SELECT COUNT(*) AS n FROM created_during_wait", &[])
            });
            pending_snapshot(records.as_ref(), candidate, &cancellation);
            let publication = writer.engine.sql(
                "CREATE TABLE created_during_wait(v INTEGER); INSERT INTO created_during_wait VALUES (2); COMMIT",
                &[],
            );
            if publication.is_err() {
                cancellation.cancel();
            }
            publication.unwrap();
            assert_eq!(query.join().unwrap().unwrap().rows[0]["n"], Value::Int(0));
        });
        assert_eq!(participant(&reader), Some(candidate));
        assert_eq!(
            reader
                .sql("SELECT COUNT(*) AS n FROM created_during_wait")
                .rows[0]["n"],
            Value::Int(0)
        );
        reader.sql("COMMIT");
        assert_eq!(
            reader
                .sql("SELECT COUNT(*) AS n FROM created_during_wait")
                .rows[0]["n"],
            Value::Int(1)
        );
    }
}

#[test]
fn cancelling_deferrable_sql_releases_admission_and_preserves_query_cancellation() {
    for (provider, cursor) in (0..3).flat_map(|provider| [(provider, false), (provider, true)]) {
        let directory = tempfile::tempdir().unwrap();
        let (writer, records) = observed_fixture(provider, &directory.path().join("cancel.redb"));
        let reader = writer.sibling();
        writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
        let previous = participant(&writer).unwrap();
        let candidate = SerializableTransactionId::new(
            previous.database(),
            previous.coordinator(),
            previous.allocation() + 1,
        )
        .unwrap();
        reader.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE");
        let cancellation = reader.engine.cancellation_token();
        std::thread::scope(|scope| {
            let query = scope.spawn(|| {
                if cursor {
                    reader
                        .engine
                        .sql_cursor("SELECT v FROM right_t", &[])
                        .map(|_| ())
                } else {
                    reader.engine.sql("SELECT v FROM right_t", &[]).map(|_| ())
                }
            });
            pending_snapshot(records.as_ref(), candidate, &cancellation);
            cancellation.cancel();
            assert_eq!(query.join().unwrap().unwrap_err().sqlstate(), Some("57014"));
        });
        cancellation.reset();
        reader.sql("ROLLBACK");
        writer.sql("COMMIT");
        reader.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE; SELECT v FROM right_t; COMMIT");
    }
}

#[test]
fn read_only_serializable_keeps_temporary_dml_and_maintenance_permissions() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        a.sql("CREATE TEMP TABLE local_t (v INTEGER); INSERT INTO local_t VALUES (1)");
        a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE; SELECT v FROM left_t; UPDATE local_t SET v = 2; ANALYZE left_t; COMMIT");
        assert_eq!(a.sql("SELECT v FROM local_t").rows[0]["v"], Value::Int(2));
        a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY");
        assert_eq!(
            a.engine
                .sql("UPDATE left_t SET v = 3", &[])
                .unwrap_err()
                .sqlstate(),
            Some("25006")
        );
        a.sql("ROLLBACK");
    }
}
