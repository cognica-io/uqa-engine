//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::{atomic::Ordering, Arc};
use uqa_core::Value;
use uqa_sql::SQLError;

fn execute_at_message_clock(engine: &Engine, clock: i64, sql: &str) -> uqa_sql::SQLResult {
    engine
        .session
        .statement_started_at_micros
        .store(clock, Ordering::Relaxed);
    uqa_execution::statement::batch::execute(&engine.batch_execution_context(), sql, &[]).unwrap()
}

#[test]
fn cached_temporal_inputs_read_each_messages_transaction_clock() {
    let engine = Engine::new();
    let _caller_clock = uqa_sql::expr::TransactionClockScope::enter(5);
    let sql = "SELECT 'now'::timestamp AS input_clock, now()::timestamp AS transaction_clock, now() = 'now' AS implicit_input";
    for clock in [90_123_456_789, 190_123_456_789] {
        let result = execute_at_message_clock(&engine, clock, sql);
        let expected = Value::Temporal(uqa_core::TemporalValue::Timestamp { micros: clock });
        assert_eq!(result.rows[0]["input_clock"], expected);
        assert_eq!(result.rows[0]["transaction_clock"], expected);
        assert_eq!(result.rows[0]["implicit_input"], Value::Bool(true));
        assert!(engine.cached_sql_statement(sql).is_some());
        assert!(engine.cached_optimized_sql_plan(sql).is_none());
        assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(5));
    }
}

#[test]
fn cached_cursor_inputs_read_each_messages_transaction_clock() {
    let engine = Engine::new();
    let sql = "SELECT 'now'::timestamp AS value";
    for clock in [90_123_456_789, 190_123_456_789] {
        engine
            .session
            .statement_started_at_micros
            .store(clock, Ordering::Relaxed);
        let cursor =
            uqa_execution::statement::cursor::execute(&engine.batch_execution_context(), sql, &[])
                .unwrap();
        let rows = cursor
            .flat_map(|batch| batch.unwrap().into_rows())
            .collect::<Vec<_>>();
        assert_eq!(
            rows[0]["value"],
            Value::Temporal(uqa_core::TemporalValue::Timestamp { micros: clock })
        );
        assert!(engine.cached_sql_statement(sql).is_some());
        assert!(engine.cached_optimized_sql_plan(sql).is_none());
    }
}

#[test]
fn immutable_inputs_reuse_optimized_plans_while_parameter_types_are_reanalyzed() {
    let engine = Engine::new();
    let sql = "SELECT '42'::integer AS value";
    engine.sql(sql, &[]).unwrap();
    let first = engine.cached_optimized_sql_plan(sql).unwrap();
    engine.sql(sql, &[]).unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &engine.cached_optimized_sql_plan(sql).unwrap()
    ));
    for value in [
        uqa_sql::SQLParam::Scalar(Value::Int(7)),
        uqa_sql::SQLParam::Scalar(Value::Str("text".into())),
    ] {
        engine.sql("SELECT $1 AS value", &[value]).unwrap();
        assert!(engine
            .cached_optimized_sql_plan("SELECT $1 AS value")
            .is_none());
    }
}

#[test]
fn simple_query_transaction_boundaries_refresh_temporal_input_clocks() {
    fn verify(engine: &Engine) {
        let _caller_clock = uqa_sql::expr::TransactionClockScope::enter(11);
        for (boundary, new_transaction) in [
            ("COMMIT", true),
            ("ROLLBACK", true),
            ("COMMIT AND CHAIN", true),
            ("ROLLBACK AND CHAIN", true),
            ("COMMIT; BEGIN", true),
            (
                "SAVEPOINT clock_savepoint; ROLLBACK TO clock_savepoint; RELEASE clock_savepoint",
                false,
            ),
            ("BEGIN", false),
        ] {
            engine.sql("BEGIN", &[]).unwrap();
            engine.session.transactions.lock()[0].started_at_micros = 90_123_456_789;
            let select = "SELECT 'now'::timestamp = now()::timestamp AS same_clock, now() = statement_timestamp() AS message_clock";
            let mut clocks = Vec::new();
            engine
                .sql_simple_query(&format!("{select}; {boundary}; {select}"), &[], |result| {
                    clocks.extend(
                        result
                            .rows
                            .iter()
                            .map(|row| (row["same_clock"].clone(), row["message_clock"].clone())),
                    );
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                clocks,
                [
                    (Value::Bool(true), Value::Bool(false)),
                    (Value::Bool(true), Value::Bool(new_transaction)),
                ],
                "{boundary}"
            );
            assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(11));
            if engine.transaction_depth() != 0 {
                engine.sql("ROLLBACK", &[]).unwrap();
            }
        }
    }

    verify(&Engine::new());
    let directory = tempfile::tempdir().unwrap();
    verify(&Engine::open(&directory.path().join("clock-boundaries.db")).unwrap());
}

#[test]
fn consumer_failure_restores_statement_clock_and_depth_after_rollback() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE pending (id BIGINT)", &[]).unwrap();
    let _caller_clock = uqa_sql::expr::TransactionClockScope::enter(17);
    engine
        .session
        .statement_started_at_micros
        .store(42, Ordering::Relaxed);
    let error = engine
        .sql_simple_query(
            "INSERT INTO pending VALUES (1); SELECT id FROM pending",
            &[],
            |_| {
                assert_eq!(
                    uqa_sql::expr::transaction_clock_micros(),
                    Some(engine.transaction_timestamp_micros())
                );
                Err(SQLError::Internal("consumer stopped".into()))
            },
        )
        .unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "consumer stopped"));
    assert_eq!(
        engine.runtime.sql_execution_depth.load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        engine
            .session
            .statement_started_at_micros
            .load(Ordering::Relaxed),
        42
    );
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(17));
    assert_eq!(
        engine
            .sql("SELECT id FROM pending", &[])
            .unwrap()
            .rows
            .len(),
        0
    );
}

#[test]
fn nested_callback_query_retains_the_outer_statement_scope() {
    let engine = Arc::new(Engine::new());
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function("nested_scope", move |_args: &[Value]| {
            let engine = weak.upgrade().unwrap();
            let clock = engine
                .session
                .statement_started_at_micros
                .load(Ordering::Relaxed);
            assert_eq!(
                engine.runtime.sql_execution_depth.load(Ordering::Relaxed),
                1
            );
            let transaction_clock = engine.transaction_timestamp_micros();
            assert_eq!(
                uqa_sql::expr::transaction_clock_micros(),
                Some(transaction_clock)
            );
            for _ in 0..2 {
                assert_eq!(
                    engine
                        .sql(
                            "SELECT 'now'::timestamp = now()::timestamp AS same_clock",
                            &[],
                        )?
                        .rows[0]["same_clock"],
                    Value::Bool(true)
                );
                assert_eq!(
                    uqa_sql::expr::transaction_clock_micros(),
                    Some(transaction_clock)
                );
            }
            assert_eq!(
                engine.runtime.sql_execution_depth.load(Ordering::Relaxed),
                1
            );
            assert_eq!(
                engine
                    .session
                    .statement_started_at_micros
                    .load(Ordering::Relaxed),
                clock
            );
            Ok(Value::Int(clock))
        })
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine.session.transactions.lock()[0].started_at_micros = 90_123_456_789;
    let _caller_clock = uqa_sql::expr::TransactionClockScope::enter(23);
    engine
        .session
        .statement_started_at_micros
        .store(73, Ordering::Relaxed);
    assert_eq!(
        engine
            .sql("COMMIT; SELECT nested_scope()", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(23));
    assert_eq!(
        engine.runtime.sql_execution_depth.load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        engine
            .session
            .statement_started_at_micros
            .load(Ordering::Relaxed),
        73
    );
}

#[test]
fn query_for_portals_retain_a_read_only_statement_scope_and_clean_up_after_failure() {
    fn verify(engine: &Engine) {
        engine.sql("CREATE TABLE loop_input(v integer); INSERT INTO loop_input VALUES (1), (2); CREATE FUNCTION loop_reader() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE rec record; total integer := 0; BEGIN FOR rec IN SELECT v FROM loop_input ORDER BY v LOOP total := total + rec.v; END LOOP; RETURN total; END $$; CREATE FUNCTION loop_failure() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE rec record; BEGIN FOR rec IN SELECT v FROM loop_input LOOP RAISE EXCEPTION 'loop failed'; END LOOP; RETURN 0; END $$", &[]).unwrap();
        for _ in 0..2 {
            assert_eq!(
                engine
                    .sql("SELECT loop_reader() AS total", &[])
                    .unwrap()
                    .rows[0]["total"],
                Value::Int(3)
            );
            assert_eq!(engine.transaction_depth(), 0);
            let cursor = engine
                .sql_cursor("SELECT loop_reader() AS total", &[])
                .unwrap();
            assert_eq!(cursor.row_count(), 1);
            drop(cursor);
            assert_eq!(engine.transaction_depth(), 0);
        }
        engine.sql("BEGIN READ ONLY", &[]).unwrap();
        assert_eq!(
            engine
                .sql("SELECT loop_reader() AS total", &[])
                .unwrap()
                .rows[0]["total"],
            Value::Int(3)
        );
        assert_eq!(engine.transaction_depth(), 1);
        assert!(engine.current_transaction_is_read_only());
        engine.sql("COMMIT", &[]).unwrap();
        assert_eq!(
            engine
                .sql("SELECT loop_failure()", &[])
                .unwrap_err()
                .sqlstate(),
            Some("P0001")
        );
        assert_eq!(engine.transaction_depth(), 0);
        assert!(engine.session.portals.lock().is_empty());
        assert_eq!(
            engine
                .sql("SELECT loop_reader() AS total", &[])
                .unwrap()
                .rows[0]["total"],
            Value::Int(3)
        );
    }
    verify(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, _) = crate::tests::relation_lock_support::sessions(provider);
        verify(&engine);
    }
}
