//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::{atomic::Ordering, Arc};
use uqa_core::Value;
use uqa_sql::SQLError;

#[test]
fn consumer_failure_restores_statement_clock_and_depth_after_rollback() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE pending (id BIGINT)", &[]).unwrap();
    engine
        .session
        .statement_started_at_micros
        .store(42, Ordering::Relaxed);
    let error = engine
        .sql_simple_query(
            "INSERT INTO pending VALUES (1); SELECT id FROM pending",
            &[],
            |_| Err(SQLError::Internal("consumer stopped".into())),
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
    assert!(engine
        .sql("SELECT id FROM pending", &[])
        .unwrap()
        .rows
        .is_empty());
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
            assert_eq!(engine.sql("SELECT 2", &[])?.rows.len(), 1);
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
    engine
        .session
        .statement_started_at_micros
        .store(73, Ordering::Relaxed);
    assert_eq!(
        engine.sql("SELECT nested_scope()", &[]).unwrap().rows.len(),
        1
    );
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
