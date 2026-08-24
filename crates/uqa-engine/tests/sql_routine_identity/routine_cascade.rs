//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement must fail")
        .sqlstate()
        .expect("SQLSTATE")
        .to_string()
}

#[test]
fn sql_standard_routine_dependencies_restrict_and_cascade_transitively() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION cascade_base(i integer) RETURNS integer RETURN i + 1",
        "CREATE FUNCTION cascade_middle(i integer) RETURNS integer RETURN cascade_base(i)",
        "CREATE FUNCTION cascade_leaf(i integer) RETURNS integer RETURN cascade_middle(i)",
        "CREATE VIEW cascade_leaf_view AS SELECT cascade_leaf(1) AS value",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let error = engine
        .sql("DROP FUNCTION cascade_base(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    assert_eq!(
        scalar(&engine, "SELECT cascade_leaf(1) AS v"),
        Value::Int(2)
    );

    engine
        .sql("DROP FUNCTION cascade_base(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![("NOTICE".into(), "drop cascades to 3 other objects".into())]
    );
    for sql in [
        "SELECT cascade_base(1)",
        "SELECT cascade_middle(1)",
        "SELECT cascade_leaf(1)",
        "SELECT * FROM cascade_leaf_view",
    ] {
        assert!(engine.sql(sql, &[]).is_err(), "{sql}");
    }
}

#[test]
fn explicit_multi_target_drop_satisfies_internal_dependency_without_cascade() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION multi_base(i integer) RETURNS integer RETURN i + 1",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION multi_dep(i integer) RETURNS integer RETURN multi_base(i)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "DROP FUNCTION multi_base(integer), multi_dep(integer) RESTRICT",
            &[],
        )
        .unwrap();
    assert_eq!(sqlstate(&engine, "SELECT multi_base(1)"), "42883");
    assert_eq!(sqlstate(&engine, "SELECT multi_dep(1)"), "42883");
}

#[test]
fn sql_string_body_keeps_postgresql_dynamic_dependency_behavior() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION dynamic_base(i integer) RETURNS integer RETURN i + 1",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION dynamic_dep(i integer) RETURNS integer LANGUAGE SQL AS 'SELECT dynamic_base($1)'",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP FUNCTION dynamic_base(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(sqlstate(&engine, "SELECT dynamic_dep(1)"), "42883");
}

#[test]
fn standard_body_positional_parameter_binds_the_exact_overload() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION positional_base(i integer) RETURNS integer RETURN i + 1",
        "CREATE FUNCTION positional_base(i bigint) RETURNS bigint RETURN i + 2",
        "CREATE FUNCTION positional_dep(i integer) RETURNS integer RETURN positional_base($1)",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    assert_eq!(
        scalar(&engine, "SELECT positional_dep(1) AS v"),
        Value::Int(2)
    );
    engine
        .sql("DROP FUNCTION positional_base(bigint) RESTRICT", &[])
        .unwrap();
    let error = engine
        .sql("DROP FUNCTION positional_base(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn standard_body_replacement_atomically_changes_its_dependency_set() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION replace_base_old(i integer) RETURNS integer RETURN i + 1",
        "CREATE FUNCTION replace_base_new(i integer) RETURNS integer RETURN i + 2",
        "CREATE FUNCTION replace_dep(i integer) RETURNS integer RETURN replace_base_old(i)",
        "CREATE OR REPLACE FUNCTION replace_dep(i integer) RETURNS integer RETURN replace_base_new(i)",
    ] {
        engine.sql(ddl, &[]).unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    engine
        .sql("DROP FUNCTION replace_base_old(integer) RESTRICT", &[])
        .unwrap();
    let error = engine
        .sql("DROP FUNCTION replace_base_new(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");

    let error = engine
        .sql(
            "CREATE OR REPLACE FUNCTION replace_dep(i integer) RETURNS integer RETURN missing_replace_target(i)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"), "{error}");
    assert_eq!(scalar(&engine, "SELECT replace_dep(1) AS v"), Value::Int(3));
    assert_eq!(
        sqlstate(&engine, "DROP FUNCTION replace_base_new(integer) RESTRICT"),
        "2BP01"
    );
}

#[test]
fn dependent_sql_procedure_and_durable_reopen_follow_the_same_graph() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-cascade.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE FUNCTION durable_base(i integer) RETURNS integer RETURN i + 1",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE PROCEDURE durable_proc(i integer) LANGUAGE SQL BEGIN ATOMIC SELECT durable_base(i); END",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let error = reopened
        .sql("DROP FUNCTION durable_base(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    reopened
        .sql("DROP FUNCTION durable_base(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        reopened.take_sql_notices(),
        vec![(
            "NOTICE".into(),
            "drop cascades to function public.durable_proc(integer)".into(),
        )]
    );
    assert_eq!(sqlstate(&reopened, "CALL durable_proc(1)"), "42883");
}

#[test]
fn durable_standard_body_keeps_its_creation_search_path_binding() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-search-path-cascade.db");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE SCHEMA early",
            "CREATE SCHEMA late",
            "CREATE FUNCTION early.bound_target(i integer) RETURNS integer RETURN i + 10",
            "CREATE FUNCTION late.bound_target(i integer) RETURNS integer RETURN i + 20",
            "SET search_path TO early, late, public",
            "CREATE FUNCTION bound_dep(i integer) RETURNS integer RETURN bound_target(i)",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
    }

    let reopened = Engine::open(&database).unwrap();
    reopened
        .sql("SET search_path TO late, early, public", &[])
        .unwrap();
    reopened
        .sql("DROP FUNCTION late.bound_target(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT early.bound_dep(1) AS v"),
        Value::Int(11)
    );

    let error = reopened
        .sql("DROP FUNCTION early.bound_target(integer) RESTRICT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    reopened
        .sql("DROP FUNCTION early.bound_target(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(sqlstate(&reopened, "SELECT early.bound_dep(1)"), "42883");
}
