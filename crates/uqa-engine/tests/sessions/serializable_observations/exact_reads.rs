//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact document lookups retain their selected snapshot and access-path predicates.

use super::admission::{observed_fixture, participant, pending_snapshot};
use super::*;
use uqa_core::{DocId, Value};
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;
use uqa_storage::mvcc::SerializableTransactionId;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ExactRead {
    Field,
    Primary,
    Indexed,
    Composite,
    Scan,
    Empty,
    Mismatch,
}

impl ExactRead {
    pub(crate) const ALL: [Self; 7] = [
        Self::Field,
        Self::Primary,
        Self::Indexed,
        Self::Composite,
        Self::Scan,
        Self::Empty,
        Self::Mismatch,
    ];

    pub(crate) fn read(
        self,
        engine: &Engine,
        table: &str,
        value: i64,
    ) -> Result<Option<DocId>, SQLError> {
        match self {
            Self::Field => engine.find_doc_id_by_field(table, "v", &Value::Int(value)),
            Self::Primary => engine.find_conflict(table, &["id".into()], &[Value::Int(1)]),
            Self::Indexed => {
                engine.find_conflict(table, &["k".into()], &[Value::Str(format!("key{value}"))])
            }
            Self::Composite => engine.find_conflict(
                table,
                &["v".into(), "k".into()],
                &[Value::Int(value), Value::Str(format!("key{value}"))],
            ),
            Self::Scan => engine.find_conflict(table, &["v".into()], &[Value::Int(value)]),
            Self::Empty => engine.find_conflict(table, &[], &[]),
            Self::Mismatch => engine.find_conflict(table, &["id".into()], &[]),
        }
    }

    fn expected(self, id: DocId) -> Option<DocId> {
        (!matches!(self, Self::Empty | Self::Mismatch)).then_some(id)
    }
}

fn prepare(session: &Session) {
    session.sql("CREATE TABLE lookup_t (id INTEGER PRIMARY KEY, v INTEGER, k TEXT UNIQUE); INSERT INTO lookup_t VALUES (1, 1, 'key1')");
}

#[test]
fn first_exact_lookup_selects_one_snapshot_through_peer_commits_and_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        let id = a.engine.table_doc_ids("lookup_t").unwrap()[0];
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for read in ExactRead::ALL {
                a.sql(&format!(
                    "BEGIN ISOLATION LEVEL {isolation}; SAVEPOINT before_read"
                ));
                assert!(participant(&a).is_none());
                b.sql("UPDATE lookup_t SET v = 2, k = 'key2' WHERE id = 1");
                assert_eq!(
                    read.read(&a.engine, "lookup_t", 2).unwrap(),
                    read.expected(id),
                    "{read:?}"
                );
                let original = participant(&a);
                b.sql("UPDATE lookup_t SET v = 3, k = 'key3' WHERE id = 1");
                a.sql("ROLLBACK TO before_read");
                assert_eq!(
                    read.read(&a.engine, "lookup_t", 2).unwrap(),
                    read.expected(id),
                    "{read:?}"
                );
                assert_eq!(a.sql("SELECT v FROM lookup_t").rows[0]["v"], Value::Int(2));
                assert_eq!(original.is_some(), isolation == "SERIALIZABLE");
                assert_eq!(participant(&a), original);
                a.sql("COMMIT");
                assert_eq!(
                    read.read(&a.engine, "lookup_t", 3).unwrap(),
                    read.expected(id)
                );
                assert_eq!(a.engine.transaction_depth(), 0);
            }
        }
    }
}

#[test]
fn fixed_exact_queries_merge_private_updates_inserts_deletes_and_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let id = a.engine.table_doc_ids("lookup_t").unwrap()[0];
        a.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT v FROM lookup_t; SAVEPOINT original; UPDATE lookup_t SET v = 2, k = 'key2'");
        for read in [
            ExactRead::Field,
            ExactRead::Indexed,
            ExactRead::Composite,
            ExactRead::Scan,
        ] {
            assert_eq!(
                read.read(&a.engine, "lookup_t", 1).unwrap(),
                None,
                "{read:?}"
            );
            assert_eq!(
                read.read(&a.engine, "lookup_t", 2).unwrap(),
                Some(id),
                "{read:?}"
            );
        }
        a.sql("DELETE FROM lookup_t; INSERT INTO lookup_t VALUES (2, 3, 'key3')");
        assert_eq!(
            a.engine
                .find_conflict("lookup_t", &["id".into()], &[Value::Int(1)])
                .unwrap(),
            None
        );
        let inserted = a
            .engine
            .find_conflict("lookup_t", &["id".into()], &[Value::Int(2)])
            .unwrap();
        assert!(inserted.is_some());
        for read in [
            ExactRead::Field,
            ExactRead::Indexed,
            ExactRead::Composite,
            ExactRead::Scan,
        ] {
            assert_eq!(read.read(&a.engine, "lookup_t", 2).unwrap(), None);
            assert_eq!(read.read(&a.engine, "lookup_t", 3).unwrap(), inserted);
        }
        a.sql("ROLLBACK TO original");
        for read in ExactRead::ALL {
            assert_eq!(
                read.read(&a.engine, "lookup_t", 1).unwrap(),
                read.expected(id)
            );
        }
        a.sql("ROLLBACK");
    }
}

#[test]
fn empty_exact_probes_conflict_only_with_their_selected_row_key_or_scan() {
    for primary in [false, true] {
        for matching in [false, true] {
            for reverse in [false, true] {
                let (_directory, sessions) = fixtures();
                for a in sessions {
                    prepare(&a);
                    let b = a.sibling();
                    a.begin();
                    b.begin();
                    let (columns, values) = if primary {
                        (vec!["id".into()], vec![Value::Int(99)])
                    } else {
                        (vec!["k".into()], vec![Value::Str("key99".into())])
                    };
                    assert_eq!(
                        a.engine
                            .find_conflict("lookup_t", &columns, &values)
                            .unwrap(),
                        None
                    );
                    b.sql("SELECT v FROM right_t");
                    a.sql("UPDATE right_t SET v = 2");
                    let key = if matching { 99 } else { 200 };
                    b.sql(&format!(
                        "INSERT INTO lookup_t VALUES ({key}, 2, 'key{key}')"
                    ));
                    let (winner, loser) = if reverse { (&b, &a) } else { (&a, &b) };
                    winner.sql("COMMIT");
                    let result = loser.engine.sql("COMMIT", &[]);
                    if matching {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("40001"));
                    } else {
                        result.unwrap();
                    }
                }
            }
        }
    }
}

#[test]
fn composite_candidate_rechecks_and_sequential_misses_retain_dependencies() {
    for read in [ExactRead::Field, ExactRead::Composite, ExactRead::Scan] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            // The indexed pivot already matches, while its remaining field does not.
            a.sql("UPDATE lookup_t SET k = 'key99'");
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(read.read(&a.engine, "lookup_t", 99).unwrap(), None);
            b.sql("SELECT v FROM right_t");
            a.sql("UPDATE right_t SET v = 2");
            b.sql("UPDATE lookup_t SET v = 99");
            assert_cycle(&a, &b);
        }
    }
}

#[test]
fn exact_lookup_errors_respect_failed_frames_and_table_lock_lifetime() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        for read in ExactRead::ALL {
            a.sql("BEGIN; SAVEPOINT before_read");
            assert_eq!(
                read.read(&a.engine, "missing_t", 1).unwrap_err().sqlstate(),
                Some("42P01")
            );
            assert_eq!(
                read.read(&a.engine, "lookup_t", 1).unwrap_err().sqlstate(),
                Some("25P02")
            );
            a.sql("ROLLBACK TO before_read");
            read.read(&a.engine, "lookup_t", 1).unwrap();
            b.sql("BEGIN");
            assert_eq!(
                b.engine
                    .sql("LOCK TABLE lookup_t IN ACCESS EXCLUSIVE MODE NOWAIT", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("55P03")
            );
            b.sql("ROLLBACK");
            a.sql("ROLLBACK");
            b.sql("BEGIN; LOCK TABLE lookup_t IN ACCESS EXCLUSIVE MODE NOWAIT; COMMIT");
        }
    }
}

fn deferrable(cancel: bool) {
    for read in [
        ExactRead::Field,
        ExactRead::Primary,
        ExactRead::Indexed,
        ExactRead::Empty,
        ExactRead::Mismatch,
    ] {
        for provider in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let (writer, records) =
                observed_fixture(provider, &directory.path().join("exact.redb"));
            prepare(&writer);
            let reader = writer.sibling();
            reader.sql("SET default_transaction_isolation = 'serializable'; SET default_transaction_read_only = on; SET default_transaction_deferrable = on");
            writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM lookup_t");
            let original = participant(&writer).unwrap();
            let candidate = SerializableTransactionId::new(
                original.database(),
                original.coordinator(),
                original.allocation() + 1,
            )
            .unwrap();
            let cancellation = reader.engine.cancellation_token();
            std::thread::scope(|scope| {
                let query = scope.spawn(|| read.read(&reader.engine, "lookup_t", 1));
                pending_snapshot(records.as_ref(), candidate, &cancellation);
                if cancel {
                    cancellation.cancel();
                    assert_eq!(query.join().unwrap().unwrap_err().sqlstate(), Some("57014"));
                    writer.sql("ROLLBACK");
                } else {
                    let publication = writer.engine.sql("UPDATE lookup_t SET v = 2; COMMIT", &[]);
                    if publication.is_err() {
                        cancellation.cancel();
                    }
                    publication.unwrap();
                    query.join().unwrap().unwrap();
                }
            });
            assert_eq!(reader.engine.transaction_depth(), 0);
            assert!(participant(&reader).is_none());
            cancellation.reset();
            read.read(&reader.engine, "lookup_t", 1).unwrap();
        }
    }
}

#[test]
fn exact_queries_honor_default_deferrable_admission() {
    deferrable(false);
}

#[test]
fn cancelled_exact_admission_releases_its_owned_frame() {
    deferrable(true);
}

#[test]
fn callback_exact_queries_keep_the_outer_statement_view() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let peer = a.sibling();
        let id = a.engine.table_doc_ids("lookup_t").unwrap()[0];
        let engine = Arc::new(a.engine);
        let source = Arc::downgrade(&engine);
        engine
            .register_scalar_function_with_options(
                "exact_during_publication",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |_: &[Value]| {
                    peer.sql("UPDATE lookup_t SET v = 2, k = 'key2'");
                    let source = source.upgrade().unwrap();
                    for read in ExactRead::ALL {
                        assert_eq!(
                            read.read(&source, "lookup_t", 1).unwrap(),
                            read.expected(id),
                            "{read:?}"
                        );
                    }
                    Ok(Value::Int(1))
                },
            )
            .unwrap();
        let result = engine
            .sql(
                "SELECT v, exact_during_publication() AS n FROM lookup_t",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows[0]["v"], Value::Int(1));
        assert_eq!(result.rows[0]["n"], Value::Int(1));
        assert_eq!(
            engine
                .find_doc_id_by_field("lookup_t", "v", &Value::Int(2))
                .unwrap(),
            Some(id)
        );
        assert_eq!(engine.transaction_depth(), 0);
    }
}
