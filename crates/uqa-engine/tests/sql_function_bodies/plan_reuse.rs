//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained SQL-body plans keep parameters, live rows and catalog invalidation.

use super::{sql, Engine, Value};

#[test]
fn sql_body_generic_plan_folds_immutable_work_once_and_rebuilds_after_discard() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};

    let engine = Engine::new();
    let evaluations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&evaluations);
    engine
        .register_scalar_function_with_options(
            "plan_constant_probe",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable),
            move |_args: &[Value]| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(Value::Int(7))
            },
        )
        .unwrap();
    sql(
        &engine,
        "CREATE FUNCTION plan_constant() RETURNS integer LANGUAGE sql IMMUTABLE \
                  AS 'SELECT plan_constant_probe()::integer'; \
                  CREATE FUNCTION retained_body(k integer) RETURNS integer LANGUAGE sql \
                  SET search_path=public AS 'SELECT plan_constant() + k'; \
                  SET plan_cache_mode=force_generic_plan",
    );
    evaluations.store(0, Ordering::Relaxed);
    for key in 1..=32 {
        assert_eq!(
            sql(&engine, &format!("SELECT retained_body({key}) AS value")).rows[0]["value"],
            Value::Int(key + 7)
        );
    }
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        1,
        "the generic executable must be reused across routine scopes"
    );
    sql(&engine, "DISCARD PLANS");
    assert_eq!(
        sql(&engine, "SELECT retained_body(50) AS value").rows[0]["value"],
        Value::Int(57)
    );
    assert_eq!(evaluations.load(Ordering::Relaxed), 2);
    sql(&engine, "DISCARD PLANS; SET plan_cache_mode=auto");
    for key in 1..=32 {
        assert_eq!(
            sql(&engine, &format!("SELECT retained_body({key}) AS value")).rows[0]["value"],
            Value::Int(key + 7),
        );
    }
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        8,
        "auto uses five custom plans followed by one reusable generic plan"
    );
    sql(&engine, "SET plan_cache_mode=force_custom_plan");
    for key in 1..=4 {
        assert_eq!(
            sql(&engine, &format!("SELECT retained_body({key}) AS value")).rows[0]["value"],
            Value::Int(key + 7),
        );
    }
    assert_eq!(evaluations.load(Ordering::Relaxed), 12);
    sql(&engine, "SET plan_cache_mode=force_generic_plan; \
                  CREATE OR REPLACE FUNCTION plan_constant() RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 9'");
    assert_eq!(
        sql(&engine, "SELECT retained_body(1) AS value").rows[0]["value"],
        Value::Int(10)
    );
}

#[test]
fn sql_body_plan_reuse_reads_each_calls_arguments_and_current_rows() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE plan_rows (id integer PRIMARY KEY, v integer); \
                  INSERT INTO plan_rows SELECT i, i * 10 FROM generate_series(1, 32) AS s(i); \
                  CREATE FUNCTION plan_lookup(k integer) RETURNS integer LANGUAGE sql \
                  AS 'SELECT v FROM plan_rows WHERE id = k'",
    );
    for mode in ["force_generic_plan", "force_custom_plan", "auto"] {
        sql(&engine, &format!("SET plan_cache_mode = {mode}"));
        for key in 1..=32 {
            let result = sql(&engine, &format!("SELECT plan_lookup({key}) AS value"));
            assert_eq!(result.rows[0]["value"], Value::Int(key * 10));
        }
    }
    sql(
        &engine,
        "SET plan_cache_mode = force_generic_plan; UPDATE plan_rows SET v = 999 WHERE id = 1",
    );
    assert_eq!(
        sql(&engine, "SELECT plan_lookup(1) AS value").rows[0]["value"],
        Value::Int(999)
    );
    sql(
        &engine,
        "BEGIN; ALTER TABLE plan_rows RENAME COLUMN v TO gone",
    );
    assert_eq!(
        engine
            .sql("SELECT plan_lookup(1)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    sql(&engine, "ROLLBACK");
    assert_eq!(
        sql(&engine, "SELECT plan_lookup(1) AS value").rows[0]["value"],
        Value::Int(999)
    );
    sql(
        &engine,
        "CREATE OR REPLACE FUNCTION plan_lookup(k integer) RETURNS integer LANGUAGE sql \
                  AS 'SELECT v + 1 FROM plan_rows WHERE id = k'",
    );
    assert_eq!(
        sql(&engine, "SELECT plan_lookup(1) AS value").rows[0]["value"],
        Value::Int(1000)
    );
}

#[test]
fn cached_sql_body_plans_refresh_executable_domain_constraints() {
    let engine = Engine::new();
    sql(
        &engine,
        "SET plan_cache_mode = force_generic_plan; \
                  CREATE DOMAIN positive_plan AS integer CHECK (VALUE > 0); \
                  CREATE FUNCTION domain_plan(v integer) RETURNS integer LANGUAGE sql \
                  AS 'SELECT v::positive_plan'",
    );
    assert_eq!(
        sql(&engine, "SELECT domain_plan(1) AS value").rows[0]["value"],
        Value::Int(1)
    );
    sql(
        &engine,
        "ALTER DOMAIN positive_plan DROP CONSTRAINT positive_plan_check; \
                  ALTER DOMAIN positive_plan ADD CONSTRAINT larger CHECK (VALUE > 10)",
    );
    assert_eq!(
        engine
            .sql("SELECT domain_plan(1)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(
        sql(&engine, "SELECT domain_plan(11) AS value").rows[0]["value"],
        Value::Int(11)
    );
}
