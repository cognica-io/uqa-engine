//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn execute(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).expect_err("statement should fail");
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
    )
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn relation_schema_fixture() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE relation_schema_caller",
        "CREATE ROLE relation_schema_group",
        "CREATE ROLE relation_schema_member INHERIT",
        "GRANT relation_schema_group TO relation_schema_member",
        "CREATE SCHEMA relation_schema_hidden",
        "CREATE SCHEMA relation_schema_visible",
        "REVOKE ALL ON SCHEMA relation_schema_hidden, relation_schema_visible FROM PUBLIC",
        "CREATE TABLE relation_schema_hidden.only_table(value integer)",
        "INSERT INTO relation_schema_hidden.only_table VALUES (1)",
        "CREATE TABLE relation_schema_visible.only_table(value integer)",
        "INSERT INTO relation_schema_visible.only_table VALUES (2)",
        "CREATE TABLE relation_schema_hidden.only_hidden(value integer)",
        "INSERT INTO relation_schema_hidden.only_hidden VALUES (7)",
        "CREATE SERVER relation_schema_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FOREIGN TABLE relation_schema_hidden.hidden_foreign(value integer) SERVER relation_schema_memory",
        "CREATE VIEW relation_schema_visible.bound_view AS SELECT value FROM relation_schema_hidden.only_hidden",
        "CREATE MATERIALIZED VIEW relation_schema_visible.bound_matview AS SELECT value FROM relation_schema_hidden.only_hidden",
        "CREATE FUNCTION relation_schema_visible.bound_function() RETURNS integer LANGUAGE SQL RETURN (SELECT value FROM relation_schema_hidden.only_hidden)",
        "CREATE FUNCTION relation_schema_visible.dynamic_invoker() RETURNS integer LANGUAGE SQL SECURITY INVOKER AS 'SELECT value FROM relation_schema_hidden.only_hidden'",
        "CREATE FUNCTION relation_schema_visible.dynamic_definer() RETURNS integer LANGUAGE SQL SECURITY DEFINER AS 'SELECT value FROM relation_schema_hidden.only_hidden'",
        "GRANT USAGE ON SCHEMA relation_schema_visible TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "SET search_path TO relation_schema_hidden, relation_schema_visible, pg_catalog",
    ] {
        execute(&engine, sql);
    }
    engine
}

#[test]
fn pg18_relation_lookup_filters_the_effective_search_path_and_checks_qualified_names() {
    let engine = relation_schema_fixture();

    assert_eq!(
        scalar(&engine, "SELECT value FROM only_table"),
        Value::Int(2)
    );
    assert_eq!(failure(&engine, "SELECT value FROM only_hidden").0, "42P01");
    assert_eq!(
        failure(&engine, "SELECT value FROM relation_schema_missing.absent").0,
        "42P01"
    );
    for sql in [
        "SELECT value FROM relation_schema_hidden.only_hidden",
        "SELECT value FROM relation_schema_hidden.absent",
        "SELECT missing_column FROM relation_schema_hidden.only_hidden",
        "SELECT value FROM relation_schema_hidden.only_hidden WHERE missing_column = 1",
        "SELECT value FROM relation_schema_hidden.hidden_foreign",
        "SELECT 'relation_schema_hidden.only_hidden'::regclass",
        "SELECT to_regclass('relation_schema_hidden.only_hidden')",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema relation_schema_hidden",
            "{sql}"
        );
    }
}

#[test]
fn pg18_mutation_and_relation_ddl_resolve_the_namespace_before_object_details() {
    let engine = relation_schema_fixture();

    for sql in [
        "INSERT INTO relation_schema_hidden.only_hidden(missing_column) VALUES (1)",
        "UPDATE relation_schema_hidden.only_hidden SET missing_column = 1",
        "DELETE FROM relation_schema_hidden.only_hidden WHERE missing_column = 1",
        "TRUNCATE relation_schema_hidden.only_hidden",
        "ALTER TABLE relation_schema_hidden.only_hidden DROP COLUMN missing_column",
        "DROP TABLE relation_schema_hidden.only_hidden",
        "CREATE VIEW relation_schema_visible.denied_view AS SELECT value FROM relation_schema_hidden.only_hidden",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema relation_schema_hidden",
            "{sql}"
        );
    }
}

#[test]
fn pg18_stored_relation_identities_do_not_repeat_namespace_name_checks() {
    let engine = relation_schema_fixture();

    assert_eq!(
        scalar(
            &engine,
            "SELECT value FROM relation_schema_visible.bound_view"
        ),
        Value::Int(7)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT value FROM relation_schema_visible.bound_matview"
        ),
        Value::Int(7)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relation_schema_visible.bound_function() AS value"
        ),
        Value::Int(7)
    );
    assert_eq!(
        failure(
            &engine,
            "SELECT relation_schema_visible.dynamic_invoker() AS value"
        )
        .0,
        "42501"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relation_schema_visible.dynamic_definer() AS value"
        ),
        Value::Int(7)
    );
}

#[test]
fn pg18_prepared_and_inherited_relation_namespace_access_tracks_live_acl_state() {
    let engine = relation_schema_fixture();
    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "PREPARE relation_schema_prepared AS SELECT value FROM relation_schema_hidden.only_hidden",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "EXECUTE relation_schema_prepared"),
        Value::Int(7)
    );
    for sql in [
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        failure(&engine, "EXECUTE relation_schema_prepared").0,
        "42501"
    );

    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "BEGIN",
        "DECLARE relation_schema_cursor CURSOR FOR SELECT value FROM relation_schema_hidden.only_hidden",
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "FETCH relation_schema_cursor"),
        Value::Int(7)
    );
    execute(&engine, "ROLLBACK");

    for sql in [
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_group",
        "SET ROLE relation_schema_member",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT value FROM relation_schema_hidden.only_hidden"
        ),
        Value::Int(7)
    );
}
