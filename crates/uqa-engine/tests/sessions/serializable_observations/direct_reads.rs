//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public document reads share SQL snapshot admission, conflict detection and completion.

use super::admission::{observed_fixture, participant, pending_snapshot};
use super::*;
use uqa_core::{DocId, Value};
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;
use uqa_storage::mvcc::SerializableTransactionId;

fn prepare_indexed_tables(session: &Session) {
    session.sql("ALTER TABLE left_t ADD COLUMN body TEXT DEFAULT 'indexed'; ALTER TABLE right_t ADD COLUMN body TEXT DEFAULT 'indexed'; CREATE INDEX left_body ON left_t USING gin(body); CREATE INDEX right_body ON right_t USING gin(body)");
}

fn indexed_fixtures() -> (tempfile::TempDir, Vec<Session>) {
    let fixtures = super::fixtures();
    for session in &fixtures.1 {
        prepare_indexed_tables(session);
    }
    fixtures
}

#[derive(Clone, Copy, Debug)]
enum DirectRead {
    Document,
    Ids,
    Count,
}

impl DirectRead {
    const ALL: [Self; 3] = [Self::Document, Self::Ids, Self::Count];

    fn read(self, engine: &Engine, table: &str, id: DocId) -> Result<Value, SQLError> {
        match self {
            Self::Document => engine
                .get_document(table, id)
                .map(|row| row.map_or(Value::Null, |row| row["v"].clone())),
            Self::Ids => engine
                .table_doc_ids(table)
                .map(|ids| Value::Int(ids.len().try_into().unwrap())),
            Self::Count => engine
                .document_count(table)
                .map(|count| Value::Int(count.try_into().unwrap())),
        }
    }
}

#[test]
fn first_direct_read_keeps_its_snapshot_and_participant_across_savepoint_undo() {
    for first in DirectRead::ALL {
        let (_directory, fixtures) = indexed_fixtures();
        for a in fixtures {
            let b = a.sibling();
            let id = a.engine.table_doc_ids("left_t").unwrap()[0];
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SAVEPOINT before_read");
            assert!(participant(&a).is_none());
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            let value = first.read(&a.engine, "left_t", id).unwrap();
            assert_eq!(
                value,
                Value::Int(if matches!(first, DirectRead::Document) {
                    2
                } else {
                    1
                }),
                "{first:?}"
            );
            let original = participant(&a).unwrap();
            b.sql("UPDATE left_t SET v = 3 WHERE id = 1; INSERT INTO left_t (id, v) VALUES (2, 3)");
            a.sql("ROLLBACK TO before_read");
            assert_eq!(
                a.engine.get_document("left_t", id).unwrap().unwrap()["v"],
                Value::Int(2)
            );
            assert_eq!(a.engine.table_doc_ids("left_t").unwrap(), vec![id]);
            assert_eq!(a.engine.document_count("left_t").unwrap(), 1);
            assert_eq!(participant(&a), Some(original));
            assert_eq!(
                a.engine
                    .sql("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("25001")
            );
            a.sql("ROLLBACK");
            assert_eq!(
                a.engine.get_document("left_t", id).unwrap().unwrap()["v"],
                Value::Int(3)
            );
            assert_eq!(a.engine.table_doc_ids("left_t").unwrap().len(), 2);
            assert_eq!(a.engine.document_count("left_t").unwrap(), 2);
            assert_eq!(a.engine.transaction_depth(), 0);
        }
    }
}

#[test]
fn direct_point_and_empty_reads_reject_either_second_committer_of_write_skew() {
    for (read, empty) in [
        (DirectRead::Document, false),
        (DirectRead::Document, true),
        (DirectRead::Ids, true),
        (DirectRead::Count, true),
    ] {
        for reverse in [false, true] {
            let (_directory, fixtures) = indexed_fixtures();
            for a in fixtures {
                let b = a.sibling();
                let table = if empty { "empty_t" } else { "left_t" };
                let id = if empty {
                    a.sql("CREATE TABLE empty_t (id INTEGER PRIMARY KEY, v INTEGER, body TEXT); CREATE INDEX empty_body ON empty_t USING gin(body)");
                    99
                } else {
                    a.engine.table_doc_ids(table).unwrap()[0]
                };
                a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
                b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
                assert_eq!(
                    read.read(&a.engine, table, id).unwrap(),
                    if empty && matches!(read, DirectRead::Document) {
                        Value::Null
                    } else {
                        Value::Int(i64::from(!empty))
                    }
                );
                b.sql("SELECT v FROM right_t");
                a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
                if empty {
                    b.engine
                        .add_document(
                            table,
                            id,
                            [
                                ("id".into(), Value::Int(99)),
                                ("v".into(), Value::Int(2)),
                                ("body".into(), Value::Str("indexed".into())),
                            ]
                            .into_iter()
                            .collect(),
                        )
                        .unwrap();
                } else {
                    b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
                }
                let (winner, loser) = if reverse { (&b, &a) } else { (&a, &b) };
                winner.sql("COMMIT");
                assert_eq!(
                    loser
                        .engine
                        .sql("COMMIT", &[])
                        .expect_err("crossed reads and writes must form a serialization cycle")
                        .sqlstate(),
                    Some("40001"),
                    "{read:?}, empty={empty}, reverse={reverse}"
                );
                assert_eq!(loser.engine.transaction_depth(), 0);
            }
        }
    }
}

fn default_deferrable_read(cancel: bool) {
    for read in DirectRead::ALL {
        for provider in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let (writer, records) =
                observed_fixture(provider, &directory.path().join("direct-read.redb"));
            prepare_indexed_tables(&writer);
            let reader = writer.sibling();
            let id = writer.engine.table_doc_ids("right_t").unwrap()[0];
            reader.sql("SET default_transaction_isolation = 'serializable'; SET default_transaction_read_only = on; SET default_transaction_deferrable = on");
            writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
            let previous = participant(&writer).unwrap();
            let candidate = SerializableTransactionId::new(
                previous.database(),
                previous.coordinator(),
                previous.allocation() + 1,
            )
            .unwrap();
            let cancellation = reader.engine.cancellation_token();
            std::thread::scope(|scope| {
                let query = scope.spawn(|| read.read(&reader.engine, "right_t", id));
                pending_snapshot(records.as_ref(), candidate, &cancellation);
                if cancel {
                    cancellation.cancel();
                    assert_eq!(query.join().unwrap().unwrap_err().sqlstate(), Some("57014"));
                } else {
                    let publication = writer.engine.sql(
                        "UPDATE right_t SET v = 2 WHERE id = 1; INSERT INTO right_t (id, v) VALUES (2, 2); COMMIT",
                        &[],
                    );
                    if publication.is_err() {
                        cancellation.cancel();
                    }
                    publication.unwrap();
                    assert_eq!(query.join().unwrap().unwrap(), Value::Int(1), "{read:?}");
                }
            });
            assert_eq!(reader.engine.transaction_depth(), 0);
            assert!(participant(&reader).is_none());
            if cancel {
                cancellation.reset();
                writer.sql("COMMIT");
            }
            assert_eq!(
                read.read(&reader.engine, "right_t", id).unwrap(),
                Value::Int(if cancel { 1 } else { 2 })
            );
            assert_eq!(reader.engine.transaction_depth(), 0);
        }
    }
}

#[test]
fn implicit_direct_reads_use_default_deferrable_admission_and_finish_their_frame() {
    default_deferrable_read(false);
}

#[test]
fn cancelled_implicit_direct_reads_release_admission_and_recover() {
    default_deferrable_read(true);
}

#[test]
fn direct_read_errors_abort_the_active_frame_and_allow_savepoint_recovery() {
    for read in DirectRead::ALL {
        let (_directory, fixtures) = indexed_fixtures();
        for a in fixtures {
            a.sql("BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_read");
            assert_eq!(
                read.read(&a.engine, "missing_t", 1).unwrap_err().sqlstate(),
                Some("42P01")
            );
            assert_eq!(
                read.read(&a.engine, "left_t", 1).unwrap_err().sqlstate(),
                Some("25P02")
            );
            a.sql("ROLLBACK TO before_read");
            assert_eq!(a.engine.document_count("left_t").unwrap(), 1);
            a.sql("COMMIT");
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
            assert_eq!(
                read.read(&a.engine, "missing_t", 1).unwrap_err().sqlstate(),
                Some("42P01")
            );
            assert_eq!(a.engine.transaction_depth(), 0);
            assert_eq!(a.engine.document_count("left_t").unwrap(), 1);
        }
    }
}

#[test]
fn direct_reads_retain_access_share_until_their_transaction_finishes() {
    for read in DirectRead::ALL {
        let (_directory, fixtures) = indexed_fixtures();
        for a in fixtures {
            let b = a.sibling();
            let id = a.engine.table_doc_ids("left_t").unwrap()[0];
            a.sql("BEGIN");
            read.read(&a.engine, "left_t", id).unwrap();
            b.sql("BEGIN");
            assert_eq!(
                b.engine
                    .sql("LOCK TABLE left_t IN ACCESS EXCLUSIVE MODE NOWAIT", &[])
                    .expect_err("direct read must retain AccessShare")
                    .sqlstate(),
                Some("55P03")
            );
            b.sql("ROLLBACK");
            a.sql("ROLLBACK");
            b.sql("BEGIN; LOCK TABLE left_t IN ACCESS EXCLUSIVE MODE NOWAIT; COMMIT");
        }
    }
}

#[test]
fn a_callback_direct_read_keeps_the_outer_read_committed_statement_snapshot() {
    let (_directory, fixtures) = indexed_fixtures();
    for a in fixtures {
        let peer = a.sibling();
        let id = a.engine.table_doc_ids("left_t").unwrap()[0];
        let engine = Arc::new(a.engine);
        let source = Arc::downgrade(&engine);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_calls = Arc::clone(&calls);
        engine
            .register_scalar_function_with_options(
                "read_during_publication",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |_: &[Value]| {
                    peer.sql("UPDATE left_t SET v = 2 WHERE id = 1");
                    observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let source = source.upgrade().unwrap();
                    Ok(source.get_document("left_t", id).unwrap().unwrap()["v"].clone())
                },
            )
            .unwrap();
        let result = engine
            .sql(
                "SELECT v, read_during_publication() AS observed FROM left_t",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows[0]["v"], Value::Int(1));
        assert_eq!(result.rows[0]["observed"], Value::Int(1));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(
            engine.get_document("left_t", id).unwrap().unwrap()["v"],
            Value::Int(2)
        );
    }
}
