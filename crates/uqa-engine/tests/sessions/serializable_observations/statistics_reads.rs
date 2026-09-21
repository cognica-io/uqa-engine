//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics and standalone planning retain their caller's transaction.

use super::admission::{observed_fixture, participant, pending_snapshot};
use super::catalog_reads::CatalogError;
use super::*;
use uqa_core::Value;
use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;
use uqa_storage::mvcc::SerializableTransactionId;

#[derive(Clone, Copy, Debug)]
pub(crate) enum StatisticsRead {
    Columns,
    TryColumns,
    TextIndex,
    Plan,
    EmptyPlan,
}

impl StatisticsRead {
    pub(crate) const ALL: [Self; 5] = [
        Self::Columns,
        Self::TryColumns,
        Self::TextIndex,
        Self::Plan,
        Self::EmptyPlan,
    ];

    pub(crate) fn read(self, engine: &Engine) -> Result<(), CatalogError> {
        match self {
            Self::Columns => engine
                .column_stats("left_t")
                .map(|_| ())
                .map_err(Into::into),
            Self::TryColumns => engine
                .try_column_stats("left_t")
                .map(|_| ())
                .map_err(Into::into),
            Self::TextIndex => engine
                .fts_index_stats(Some("left_t"))
                .map(|_| ())
                .map_err(Into::into),
            Self::Plan | Self::EmptyPlan => {
                let expression = if matches!(self, Self::Plan) {
                    let statements =
                        uqa_sql::compile("SELECT id FROM left_t WHERE text_match(body, 'token')")
                            .unwrap();
                    let uqa_sql::ast::Statement::Select(statement) =
                        statements.into_iter().next().unwrap()
                    else {
                        unreachable!()
                    };
                    uqa_planner::ExpressionPlan::lower(statement.r#where.unwrap()).scalar
                } else {
                    uqa_execution::ScalarExpr::Literal(Value::Bool(true))
                };
                uqa_engine::operator_tree_bridge::optimised_tree_for(
                    engine,
                    "left_t",
                    &expression,
                    &[],
                )
                .map(|_| ())
                .map_err(Into::into)
            }
        }
    }
}

fn prepare(session: &Session) {
    session.sql("ALTER TABLE left_t ADD COLUMN body TEXT DEFAULT 'token'; CREATE INDEX left_body ON left_t USING gin(body)");
    session.engine.run_analyze(Some("left_t")).unwrap();
}

#[test]
fn first_statistics_query_retains_the_data_snapshot_through_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for read in StatisticsRead::ALL {
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
                    "{isolation}: {read:?}"
                );
                assert_eq!(original.is_some(), isolation == "SERIALIZABLE");
                assert_eq!(participant(&a), original);
                a.sql("COMMIT");
                assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(3));
            }
        }
    }
}

#[test]
fn statistics_lookup_errors_abort_only_the_active_savepoint() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        for missing in [StatisticsRead::Columns, StatisticsRead::TextIndex] {
            a.sql(
                "BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_read; UPDATE right_t SET v = 3",
            );
            let error: CatalogError = match missing {
                StatisticsRead::Columns => a.engine.column_stats("missing_t").unwrap_err().into(),
                _ => a
                    .engine
                    .fts_index_stats(Some("missing_t"))
                    .unwrap_err()
                    .into(),
            };
            assert!(a.engine.transaction_failed(), "{error:?}");
            let expected = a.engine.sql("SELECT 1", &[]).unwrap_err();
            assert_eq!(expected.sqlstate(), Some("25P02"));
            for read in StatisticsRead::ALL {
                read.read(&a.engine)
                    .unwrap_err()
                    .assert_transaction_error(&expected);
            }
            b.sql("UPDATE left_t SET v = 4");
            a.sql("ROLLBACK TO before_read; COMMIT");
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
            assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(4));
            assert_eq!(a.engine.transaction_depth(), 0);
        }
    }
}

fn deferrable_statistics(cancel: bool) {
    for read in [
        StatisticsRead::Columns,
        StatisticsRead::TextIndex,
        StatisticsRead::EmptyPlan,
    ] {
        for provider in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let (writer, records) =
                observed_fixture(provider, &directory.path().join("statistics.redb"));
            let reader = writer.sibling();
            reader.sql("SET default_transaction_isolation = 'serializable'; SET default_transaction_read_only = on; SET default_transaction_deferrable = on");
            writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
            let original = participant(&writer).unwrap();
            let candidate = SerializableTransactionId::new(
                original.database(),
                original.coordinator(),
                original.allocation() + 1,
            )
            .unwrap();
            let cancellation = reader.engine.cancellation_token();
            std::thread::scope(|scope| {
                let query = scope.spawn(|| read.read(&reader.engine));
                pending_snapshot(records.as_ref(), candidate, &cancellation);
                if cancel {
                    cancellation.cancel();
                    query
                        .join()
                        .unwrap()
                        .unwrap_err()
                        .assert_transaction_error(&SQLError::from(
                            cancellation.check().unwrap_err(),
                        ));
                    writer.sql("ROLLBACK");
                } else {
                    let publication = writer.engine.sql("UPDATE left_t SET v = 2; COMMIT", &[]);
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
            read.read(&reader.engine).unwrap();
        }
    }
}

#[test]
fn statistics_queries_honor_default_deferrable_admission() {
    deferrable_statistics(false);
}

#[test]
fn cancelled_statistics_admission_releases_its_owned_frame() {
    deferrable_statistics(true);
}

fn text_length(engine: &Engine, all: bool) -> u64 {
    let stats = engine.fts_index_stats((!all).then_some("left_t")).unwrap();
    let stats = stats
        .iter()
        .find(|stats| stats.table_name == "public.left_t" && stats.field == "body")
        .unwrap();
    stats.total_field_length
}

#[test]
fn text_statistics_retain_the_selected_data_through_peer_publication() {
    for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.sql(&format!("BEGIN ISOLATION LEVEL {isolation}"));
            assert_eq!(text_length(&a.engine, false), 1);
            let original = participant(&a);
            b.sql("UPDATE left_t SET body = 'token token token' WHERE id = 1");
            for all in [false, true] {
                assert_eq!(text_length(&a.engine, all), 1);
                assert_eq!(participant(&a), original);
            }
            a.sql("COMMIT");
            assert_eq!(text_length(&a.engine, false), 3);
        }
    }
}

#[test]
fn text_statistics_in_callbacks_keep_the_outer_statement_view() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let peer = a.sibling();
        let engine = Arc::new(a.engine);
        let source = Arc::downgrade(&engine);
        engine
            .register_scalar_function_with_options(
                "statistics_during_publication",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |_: &[Value]| {
                    peer.sql("UPDATE left_t SET body = 'token token token' WHERE id = 1");
                    Ok(Value::Int(
                        text_length(&source.upgrade().unwrap(), true)
                            .try_into()
                            .unwrap(),
                    ))
                },
            )
            .unwrap();
        let result = engine
            .sql(
                "SELECT body, statistics_during_publication() AS n FROM left_t",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows[0]["body"], Value::Str("token".into()));
        assert_eq!(result.rows[0]["n"], Value::Int(1));
        assert_eq!(text_length(&engine, false), 3);
        assert_eq!(engine.transaction_depth(), 0);
    }
}

#[test]
fn planner_metadata_does_not_manufacture_user_row_dependencies() {
    for read in [
        StatisticsRead::Columns,
        StatisticsRead::TryColumns,
        StatisticsRead::Plan,
        StatisticsRead::EmptyPlan,
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
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
