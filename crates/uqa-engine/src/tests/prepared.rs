//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;

#[test]
fn prepared_temporal_constants_keep_the_preparation_clock() {
    use std::sync::atomic::Ordering;
    use uqa_core::Value;

    fn execute_at_message_clock(engine: &Engine, clock: i64, sql: &str) -> uqa_sql::SQLResult {
        engine
            .session
            .statement_started_at_micros
            .store(clock, Ordering::Relaxed);
        uqa_execution::statement::batch::execute(&engine.batch_execution_context(), sql, &[])
            .unwrap()
    }

    let engine = Engine::new();
    let _caller_clock = uqa_sql::expr::TransactionClockScope::enter(5);
    let prepared_at = 90_123_456_789;
    execute_at_message_clock(
        &engine,
        prepared_at,
        "PREPARE fixed_clock (text) AS SELECT 'now'::timestamp AS input_clock, now()::timestamp AS transaction_clock, now() = 'now' AS implicit_input, 'now'::text::timestamp AS text_clock, $1::timestamp AS parameter_clock",
    );
    for (mode, clock) in [
        ("force_generic_plan", 190_123_456_789),
        ("force_custom_plan", 290_123_456_789),
        ("force_generic_plan", 390_123_456_789),
    ] {
        engine
            .sql(&format!("SET plan_cache_mode = {mode}"), &[])
            .unwrap();
        let result = execute_at_message_clock(&engine, clock, "EXECUTE fixed_clock('now')");
        assert_eq!(
            result.rows[0]["input_clock"],
            Value::Temporal(uqa_core::TemporalValue::Timestamp {
                micros: prepared_at
            })
        );
        assert_eq!(
            result.rows[0]["transaction_clock"],
            Value::Temporal(uqa_core::TemporalValue::Timestamp { micros: clock })
        );
        assert_eq!(result.rows[0]["implicit_input"], Value::Bool(false));
        for name in ["text_clock", "parameter_clock"] {
            assert_eq!(
                result.rows[0][name],
                Value::Temporal(uqa_core::TemporalValue::Timestamp { micros: clock })
            );
        }
        assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(5));
    }
    execute_at_message_clock(&engine, 490_123_456_789, "DISCARD PLANS");
    let result = execute_at_message_clock(&engine, 590_123_456_789, "EXECUTE fixed_clock('now')");
    for name in [
        "input_clock",
        "transaction_clock",
        "text_clock",
        "parameter_clock",
    ] {
        assert_eq!(
            result.rows[0][name],
            Value::Temporal(uqa_core::TemporalValue::Timestamp {
                micros: 590_123_456_789
            })
        );
    }
    assert_eq!(result.rows[0]["implicit_input"], Value::Bool(true));
    assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(5));
}

#[test]
fn prepared_inputs_survive_local_and_sibling_data_only_refreshes() {
    fn verify(engine: &Engine) {
        use uqa_core::{TemporalValue, Value};

        engine
            .sql("CREATE TABLE prepared_clock_rows (id integer)", &[])
            .unwrap();
        let peer = engine
            .storage
            .backend
            .as_ref()
            .map(|_| engine.new_session().unwrap());
        engine.sql("BEGIN", &[]).unwrap();
        let prepared_at = 90_123_456_789;
        engine.session.transactions.lock()[0].started_at_micros = prepared_at;
        engine.sql("PREPARE saved_clock AS SELECT 'now'::timestamp AS input_clock, (SELECT count(*) FROM prepared_clock_rows) AS row_count", &[]).unwrap();
        engine
            .register_prepared(
                "direct_clock".into(),
                uqa_sql::compile("SELECT 'now'::timestamp AS input_clock")
                    .unwrap()
                    .remove(0),
            )
            .unwrap();
        let expected_clock = Value::Temporal(TemporalValue::Timestamp {
            micros: prepared_at,
        });
        assert_eq!(
            engine.sql("EXECUTE direct_clock", &[]).unwrap().rows[0]["input_clock"],
            expected_clock
        );
        let original = engine.session.prepared.read()["saved_clock"]
            .logical_plan
            .clone();
        engine.sql("COMMIT", &[]).unwrap();
        let verify_rows = |count| {
            assert_eq!(
                engine.sql("EXECUTE direct_clock", &[]).unwrap().rows[0]["input_clock"],
                expected_clock
            );
            let result = engine.sql("EXECUTE saved_clock", &[]).unwrap();
            assert_eq!(
                result.rows[0]["input_clock"],
                Value::Temporal(TemporalValue::Timestamp {
                    micros: prepared_at
                })
            );
            assert_eq!(result.rows[0]["row_count"], Value::Int(count));
            let current = &engine.session.prepared.read()["saved_clock"];
            assert!(std::sync::Arc::ptr_eq(&original, &current.logical_plan));
            assert!(!current.needs_analysis);
        };
        verify_rows(0);
        for (sql, count) in [
            ("INSERT INTO prepared_clock_rows VALUES (1)", 1),
            ("UPDATE prepared_clock_rows SET id=2", 1),
            ("DELETE FROM prepared_clock_rows", 0),
        ] {
            engine.sql(sql, &[]).unwrap();
            verify_rows(count);
        }
        engine
            .sql(
                "BEGIN; INSERT INTO prepared_clock_rows VALUES (4); ROLLBACK",
                &[],
            )
            .unwrap();
        verify_rows(0);
        engine
            .sql("INSERT INTO prepared_clock_rows VALUES (5)", &[])
            .unwrap();
        engine
            .sql("BEGIN; SAVEPOINT before_update; UPDATE prepared_clock_rows SET id=6; ROLLBACK TO SAVEPOINT before_update", &[])
            .unwrap();
        verify_rows(1);
        assert_eq!(
            engine
                .sql("SELECT id FROM prepared_clock_rows", &[])
                .unwrap()
                .rows[0]["id"],
            Value::Int(5)
        );
        engine.sql("COMMIT", &[]).unwrap();
        verify_rows(1);
        if let Some(peer) = peer {
            peer.sql("INSERT INTO prepared_clock_rows VALUES (3)", &[])
                .unwrap();
            verify_rows(2);
        }
    }

    verify(&Engine::new());
    let directory = tempfile::tempdir().unwrap();
    verify(&Engine::open(&directory.path().join("prepared-clocks.db")).unwrap());
}

#[test]
fn prepared_costs_distinguish_rare_and_unknown_index_keys() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE cost_distribution (bucket integer)",
        "INSERT INTO cost_distribution SELECT CASE WHEN n=10000 THEN 2 ELSE 1 END FROM generate_series(1,10000) g(n)",
        "CREATE INDEX cost_bucket ON cost_distribution (bucket)",
        "ANALYZE cost_distribution",
        "PREPARE cost_query(integer) AS SELECT count(*) FROM cost_distribution WHERE bucket=$1",
        "EXECUTE cost_query(2)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let entry = engine.session.prepared.read()["cost_query"].clone();
    let generic = crate::capabilities::statement_planning::optimize_engine_plan(
        &engine,
        (*entry.logical_plan).clone(),
    )
    .unwrap();
    let generic_cost =
        crate::capabilities::statement_planning::estimate_engine_plan(&engine, &generic).unwrap();
    let stats = engine.try_query_column_stats("cost_distribution").unwrap();
    assert!(
        entry.total_custom_cost < generic_cost.execution,
        "custom={} generic={generic_cost:?} statistics={stats:?} plan={generic:?}",
        entry.total_custom_cost
    );
}

mod ownership;
