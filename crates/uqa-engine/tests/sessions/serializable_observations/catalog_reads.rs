//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public table metadata queries enter the same transaction as subsequent row reads.

use super::admission::{observed_fixture, participant, pending_snapshot};
use super::*;
use uqa_core::Value;
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};
use uqa_storage::mvcc::SerializableTransactionId;
use uqa_storage::StorageBackendResult;

#[derive(Clone, Copy, Debug)]
enum TableMetadataRead {
    HasTable,
    TryHasTable,
    Columns,
    TryColumns,
    HasColumn,
    TryHasColumn,
    Names,
    Description,
    TryDescription,
}

impl TableMetadataRead {
    const ALL: [Self; 9] = [
        Self::HasTable,
        Self::TryHasTable,
        Self::Columns,
        Self::TryColumns,
        Self::HasColumn,
        Self::TryHasColumn,
        Self::Names,
        Self::Description,
        Self::TryDescription,
    ];

    fn read(self, engine: &Engine) -> StorageBackendResult<()> {
        match self {
            Self::HasTable => engine.has_table("left_t").map(|_| ()),
            Self::TryHasTable => engine.try_has_table("left_t").map(|_| ()),
            Self::Columns => engine.table_columns("left_t").map(|_| ()),
            Self::TryColumns => engine.try_table_columns("left_t").map(|_| ()),
            Self::HasColumn => engine.table_has_column("left_t", "v").map(|_| ()),
            Self::TryHasColumn => engine.try_table_has_column("left_t", "v").map(|_| ()),
            Self::Names => engine.table_names().map(|_| ()),
            Self::Description => engine.describe_table("left_t").map(|_| ()),
            Self::TryDescription => engine.try_describe_table("left_t").map(|_| ()),
        }
    }
}

#[test]
fn first_table_metadata_query_retains_the_data_snapshot_through_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        let b = a.sibling();
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for read in TableMetadataRead::ALL {
                a.sql(&format!(
                    "BEGIN ISOLATION LEVEL {isolation}; SAVEPOINT before_read"
                ));
                assert!(participant(&a).is_none());
                b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
                read.read(&a.engine).unwrap();
                let original = participant(&a);
                b.sql("UPDATE left_t SET v = 3 WHERE id = 1");
                a.sql("ROLLBACK TO before_read");
                assert_eq!(
                    a.sql("SELECT v FROM left_t").rows[0]["v"],
                    Value::Int(2),
                    "{isolation}: {read:?} did not retain the first query's snapshot"
                );
                assert_eq!(
                    original.is_some(),
                    isolation == "SERIALIZABLE",
                    "{isolation}: {read:?} did not use the expected admission"
                );
                assert_eq!(participant(&a), original);
                a.sql("COMMIT");
                assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(3));
            }
        }
    }
}

#[test]
fn metadata_query_failures_abort_only_the_active_savepoint() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        let b = a.sibling();
        a.sql("BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_read; UPDATE right_t SET v = 3");
        a.engine.table_columns("missing_t").unwrap_err();
        assert!(a.engine.transaction_failed());
        for read in TableMetadataRead::ALL {
            let error = read.read(&a.engine).unwrap_err();
            assert_eq!(
                uqa_execution::storage_errors::storage_error("metadata query", &error).sqlstate(),
                Some("25P02"),
                "{read:?}: {error}"
            );
        }
        b.sql("UPDATE left_t SET v = 4");
        a.sql("ROLLBACK TO before_read");
        for read in TableMetadataRead::ALL {
            read.read(&a.engine).unwrap();
        }
        a.sql("COMMIT");
        assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
        assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(4));
        a.engine.table_columns("missing_t").unwrap_err();
        assert_eq!(a.engine.transaction_depth(), 0);
        assert!(!a.engine.transaction_failed());
    }
}

#[test]
fn table_metadata_queries_do_not_manufacture_user_row_dependencies() {
    for read in TableMetadataRead::ALL {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let b = a.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            read.read(&a.engine).unwrap();
            b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM right_t; UPDATE left_t SET v = 2; COMMIT");
            a.sql("UPDATE right_t SET v = 2; COMMIT");
            assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(2));
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
        }
    }
}

fn deferrable_metadata_query(cancel: bool) {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let (writer, records) = observed_fixture(provider, &directory.path().join("metadata.redb"));
        let reader = writer.sibling();
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
            let query = scope.spawn(|| reader.engine.table_names());
            pending_snapshot(records.as_ref(), candidate, &cancellation);
            if cancel {
                cancellation.cancel();
                let error = query.join().unwrap().unwrap_err();
                assert_eq!(
                    uqa_execution::storage_errors::storage_error("metadata query", &error)
                        .sqlstate(),
                    Some("57014")
                );
                writer.sql("ROLLBACK");
            } else {
                let publication = writer.engine.sql("UPDATE left_t SET v = 2; COMMIT", &[]);
                if publication.is_err() {
                    cancellation.cancel();
                }
                publication.unwrap();
                assert_eq!(query.join().unwrap().unwrap().len(), 2);
            }
        });
        assert_eq!(reader.engine.transaction_depth(), 0);
        assert!(participant(&reader).is_none());
        cancellation.reset();
        assert!(reader.engine.has_table("left_t").unwrap());
    }
}

#[test]
fn implicit_metadata_queries_honor_default_deferrable_admission() {
    deferrable_metadata_query(false);
}

#[test]
fn cancelled_metadata_admission_releases_its_implicit_transaction() {
    deferrable_metadata_query(true);
}

#[test]
fn metadata_queries_in_callbacks_keep_the_outer_statement_snapshot() {
    for read in TableMetadataRead::ALL {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            let peer = a.sibling();
            let id = a.engine.table_doc_ids("left_t").unwrap()[0];
            let engine = Arc::new(a.engine);
            let source = Arc::downgrade(&engine);
            engine
                .register_scalar_function_with_options(
                    "metadata_during_publication",
                    SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                    move |_: &[Value]| {
                        peer.sql("UPDATE left_t SET v = 2");
                        let source = source.upgrade().unwrap();
                        read.read(&source).unwrap();
                        Ok(source.get_document("left_t", id).unwrap().unwrap()["v"].clone())
                    },
                )
                .unwrap();
            let result = engine
                .sql(
                    "SELECT v, metadata_during_publication() AS observed FROM left_t",
                    &[],
                )
                .unwrap();
            assert_eq!(result.rows[0]["v"], Value::Int(1), "{read:?}");
            assert_eq!(result.rows[0]["observed"], Value::Int(1), "{read:?}");
            assert_eq!(engine.transaction_depth(), 0);
        }
    }
}
