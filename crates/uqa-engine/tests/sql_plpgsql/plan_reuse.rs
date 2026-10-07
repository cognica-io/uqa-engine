//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural plans retain first-use types and read current invocation values.

use super::{engine, exec, exec_err, scalar, Value};

#[test]
fn prepared_record_field_rejects_changed_type_when_evaluated() {
    let engine = engine();
    exec(
        &engine,
        "CREATE FUNCTION change_record() RETURNS text LANGUAGE plpgsql AS $$
        DECLARE r record; x text; i integer;
        BEGIN FOR i IN 1..2 LOOP
            IF i=1 THEN SELECT 1::integer AS v INTO r;
            ELSE SELECT 'two'::text AS v INTO r; END IF;
            SELECT r.v::text INTO x;
        END LOOP; RETURN x; END$$",
    );
    let error = exec_err(&engine, "SELECT change_record()");
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(
        error.to_string(),
        "type of parameter 6 (text) does not match that when preparing the plan (integer)"
    );
}

#[test]
fn procedural_generic_plans_fold_immutable_work_once_and_keep_dynamic_sql_uncached() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uqa_engine::{SQLFunctionOptions, SQLFunctionVolatility};

    let engine = engine();
    let evaluations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&evaluations);
    engine
        .register_scalar_function_with_options(
            "plan_constant_probe",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable),
            move |_: &[Value]| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(Value::Int(7))
            },
        )
        .unwrap();
    exec(&engine, "CREATE FUNCTION plan_constant() RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT plan_constant_probe()::integer';
        CREATE FUNCTION retained_loop() RETURNS integer LANGUAGE plpgsql AS $$
        DECLARE i integer; v integer;
        BEGIN FOR i IN 1..32 LOOP SELECT plan_constant() + i INTO v; END LOOP; RETURN v; END$$;
        CREATE FUNCTION dynamic_loop() RETURNS integer LANGUAGE plpgsql AS $$
        DECLARE i integer; v integer;
        BEGIN FOR i IN 1..32 LOOP EXECUTE 'SELECT plan_constant() + $1' INTO v USING i; END LOOP; RETURN v; END$$;
        CREATE FUNCTION retained_cursors() RETURNS integer LANGUAGE plpgsql AS $$
        DECLARE c CURSOR(k integer) FOR SELECT plan_constant() + k; i integer; v integer;
        BEGIN FOR i IN 1..32 LOOP
            OPEN c(i); FETCH c INTO v; CLOSE c;
            FOR v IN SELECT plan_constant() + i LOOP NULL; END LOOP;
        END LOOP; RETURN v; END$$");
    for (mode, expected) in [
        ("force_generic_plan", 1),
        ("auto", 6),
        ("force_custom_plan", 32),
    ] {
        exec(
            &engine,
            &format!("DISCARD PLANS; SET plan_cache_mode={mode}"),
        );
        evaluations.store(0, Ordering::Relaxed);
        assert_eq!(scalar(&engine, "SELECT retained_loop()"), Value::Int(39));
        assert_eq!(evaluations.load(Ordering::Relaxed), expected, "{mode}");
    }
    exec(
        &engine,
        "DISCARD PLANS; SET plan_cache_mode=force_generic_plan",
    );
    evaluations.store(0, Ordering::Relaxed);
    assert_eq!(scalar(&engine, "SELECT retained_cursors()"), Value::Int(39));
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        2,
        "each static cursor source owns one plan"
    );
    evaluations.store(0, Ordering::Relaxed);
    assert_eq!(scalar(&engine, "SELECT dynamic_loop()"), Value::Int(39));
    assert_eq!(
        evaluations.load(Ordering::Relaxed),
        32,
        "EXECUTE plans each dynamic command"
    );
}

#[test]
fn procedural_plan_reuse_observes_live_rows_catalog_undo_and_domain_constraints() {
    let engine = engine();
    exec(&engine, "SET plan_cache_mode=force_generic_plan;
        CREATE TABLE retained_rows(id integer PRIMARY KEY, v integer);
        INSERT INTO retained_rows SELECT i, i*10 FROM generate_series(1,32) AS s(i);
        CREATE DOMAIN retained_positive AS integer CHECK (VALUE>0);
        CREATE FUNCTION retained_lookup(k integer) RETURNS integer LANGUAGE plpgsql AS $$
        DECLARE v integer; BEGIN SELECT r.v::retained_positive INTO v FROM retained_rows AS r WHERE id=k; RETURN v; END$$");
    for key in 1..=32 {
        assert_eq!(
            scalar(&engine, &format!("SELECT retained_lookup({key})")),
            Value::Int(key * 10)
        );
    }
    exec(&engine, "UPDATE retained_rows SET v=999 WHERE id=1");
    assert_eq!(
        scalar(&engine, "SELECT retained_lookup(1)"),
        Value::Int(999)
    );
    exec(
        &engine,
        "BEGIN; ALTER TABLE retained_rows RENAME COLUMN v TO gone",
    );
    assert_eq!(
        exec_err(&engine, "SELECT retained_lookup(1)").sqlstate(),
        Some("42703")
    );
    exec(&engine, "ROLLBACK");
    assert_eq!(
        scalar(&engine, "SELECT retained_lookup(1)"),
        Value::Int(999)
    );
    exec(
        &engine,
        "ALTER DOMAIN retained_positive DROP CONSTRAINT retained_positive_check;
        ALTER DOMAIN retained_positive ADD CONSTRAINT small CHECK(VALUE<500)",
    );
    assert_eq!(
        exec_err(&engine, "SELECT retained_lookup(1)").sqlstate(),
        Some("23514")
    );
    assert_eq!(scalar(&engine, "SELECT retained_lookup(2)"), Value::Int(20));
}
