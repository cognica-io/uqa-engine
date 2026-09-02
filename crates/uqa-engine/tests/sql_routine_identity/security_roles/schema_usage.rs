//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn routine_failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).expect_err("statement should fail");
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
    )
}

fn routine_name_resolution_fixture() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE routine_schema_caller",
        "CREATE ROLE routine_schema_group",
        "CREATE ROLE routine_schema_member",
        "GRANT routine_schema_group TO routine_schema_member",
        "CREATE SCHEMA routine_schema_hidden",
        "CREATE SCHEMA routine_schema_visible",
        "REVOKE ALL ON SCHEMA routine_schema_hidden, routine_schema_visible FROM PUBLIC",
        "CREATE FUNCTION routine_schema_hidden.pick(value integer) RETURNS text LANGUAGE SQL AS 'SELECT ''hidden'''",
        "CREATE FUNCTION routine_schema_visible.pick(value integer) RETURNS text LANGUAGE SQL AS 'SELECT ''visible'''",
        "CREATE FUNCTION routine_schema_hidden.only_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 7'",
        "CREATE FUNCTION routine_schema_hidden.denied_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 8'",
        "REVOKE EXECUTE ON FUNCTION routine_schema_hidden.denied_probe() FROM PUBLIC",
        "CREATE PROCEDURE routine_schema_hidden.procedure_probe() LANGUAGE plpgsql AS 'BEGIN NULL; END'",
        "GRANT USAGE ON SCHEMA routine_schema_visible TO routine_schema_caller",
        "GRANT EXECUTE ON FUNCTION routine_schema_hidden.pick(integer), routine_schema_hidden.only_probe() TO routine_schema_caller",
        "GRANT EXECUTE ON PROCEDURE routine_schema_hidden.procedure_probe() TO routine_schema_caller",
        "SET ROLE routine_schema_caller",
        "SET search_path TO routine_schema_hidden, routine_schema_visible, pg_catalog",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

#[test]
fn pg18_routine_name_resolution_enforces_schema_usage() {
    let engine = routine_name_resolution_fixture();

    assert_eq!(
        scalar(&engine, "SELECT pick(1) AS v"),
        Value::Str("visible".into())
    );
    assert_eq!(sqlstate(&engine, "SELECT only_probe()"), "42883");
    for sql in [
        "SELECT routine_schema_hidden.pick(missing_column) FROM (SELECT 1 AS present) source",
        "SELECT 1 FROM (SELECT 1 AS present) source WHERE routine_schema_hidden.pick(missing_column) IS NOT NULL",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42703", "{sql}");
    }
    for sql in [
        "SELECT routine_schema_hidden.pick(CAST(NULL AS missing_routine_type))",
        "SELECT 1 WHERE routine_schema_hidden.pick(CAST(NULL AS missing_routine_type)) IS NOT NULL",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42704", "{sql}");
    }
    for sql in [
        "SELECT routine_schema_hidden.only_probe()",
        "SELECT routine_schema_hidden.missing_probe()",
        "CALL routine_schema_hidden.procedure_probe()",
    ] {
        let (state, message) = routine_failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema routine_schema_hidden",
            "{sql}"
        );
    }
    let (state, message) = routine_failure(&engine, "SELECT missing_routine_schema.probe()");
    assert_eq!(state, "3F000");
    assert_eq!(message, "schema \"missing_routine_schema\" does not exist");
    let (state, message) = routine_failure(&engine, "SELECT routine_schema_hidden.denied_probe()");
    assert_eq!(state, "42501");
    assert_eq!(
        message,
        "permission denied for schema routine_schema_hidden"
    );

    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SCHEMA routine_schema_hidden TO routine_schema_caller",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_schema_caller", &[]).unwrap();
    assert_eq!(
        scalar(&engine, "SELECT pick(1) AS v"),
        Value::Str("hidden".into())
    );
    let (state, message) = routine_failure(&engine, "SELECT routine_schema_hidden.denied_probe()");
    assert_eq!(state, "42501");
    assert_eq!(message, "permission denied for function denied_probe");
    engine
        .sql("CALL routine_schema_hidden.procedure_probe()", &[])
        .unwrap();
    engine
        .sql(
            "PREPARE routine_schema_prepared AS SELECT routine_schema_hidden.only_probe() AS v",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(&engine, "EXECUTE routine_schema_prepared"),
        Value::Int(7)
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "REVOKE USAGE ON SCHEMA routine_schema_hidden FROM routine_schema_caller",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_schema_caller", &[]).unwrap();
    assert_eq!(
        sqlstate(&engine, "EXECUTE routine_schema_prepared"),
        "42501"
    );

    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SCHEMA routine_schema_hidden TO routine_schema_group",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_schema_member", &[]).unwrap();
    assert_eq!(
        scalar(&engine, "SELECT routine_schema_hidden.only_probe() AS v"),
        Value::Int(7)
    );
}

#[test]
fn pg18_routine_ddl_checks_schema_usage_before_object_authority() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE routine_schema_owner",
        "CREATE ROLE routine_schema_grantee",
        "CREATE SCHEMA routine_schema_ddl",
        "REVOKE ALL ON SCHEMA routine_schema_ddl FROM PUBLIC",
        "CREATE FUNCTION routine_schema_ddl.owner_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
        "ALTER FUNCTION routine_schema_ddl.owner_probe() OWNER TO routine_schema_owner",
        "SET ROLE routine_schema_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    for sql in [
        "ALTER FUNCTION routine_schema_ddl.owner_probe() IMMUTABLE",
        "GRANT EXECUTE ON FUNCTION routine_schema_ddl.owner_probe() TO routine_schema_grantee",
        "REVOKE EXECUTE ON FUNCTION routine_schema_ddl.owner_probe() FROM PUBLIC",
        "DROP FUNCTION routine_schema_ddl.owner_probe()",
    ] {
        let (state, message) = routine_failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(message, "permission denied for schema routine_schema_ddl");
    }
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SCHEMA routine_schema_ddl TO routine_schema_owner",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_schema_owner", &[]).unwrap();
    engine
        .sql(
            "ALTER FUNCTION routine_schema_ddl.owner_probe() IMMUTABLE",
            &[],
        )
        .unwrap();
}

#[test]
fn pg18_bound_routine_identities_do_not_repeat_schema_name_checks() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE routine_bound_caller",
        "CREATE SCHEMA routine_bound_hidden",
        "CREATE SCHEMA routine_bound_visible",
        "REVOKE ALL ON SCHEMA routine_bound_hidden, routine_bound_visible FROM PUBLIC",
        "CREATE FUNCTION routine_bound_hidden.value_probe() RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT 9'",
        "CREATE VIEW routine_bound_visible.value_view AS SELECT routine_bound_hidden.value_probe() AS v",
        "CREATE TABLE routine_bound_visible.value_generated (source integer, v integer GENERATED ALWAYS AS (routine_bound_hidden.value_probe()) STORED)",
        "CREATE FUNCTION routine_bound_visible.atomic_probe() RETURNS integer LANGUAGE SQL RETURN routine_bound_hidden.value_probe()",
        "CREATE FUNCTION routine_bound_visible.invoker_probe() RETURNS integer LANGUAGE SQL SECURITY INVOKER AS 'SELECT routine_bound_hidden.value_probe()'",
        "CREATE FUNCTION routine_bound_visible.definer_probe() RETURNS integer LANGUAGE SQL SECURITY DEFINER AS 'SELECT routine_bound_hidden.value_probe()'",
        "GRANT INSERT, SELECT ON TABLE routine_bound_visible.value_generated TO routine_bound_caller",
        "GRANT USAGE ON SCHEMA routine_bound_visible TO routine_bound_caller",
        "SET ROLE routine_bound_caller",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    assert_eq!(
        scalar(&engine, "SELECT v FROM routine_bound_visible.value_view"),
        Value::Int(9)
    );
    assert_eq!(
        scalar(
            &engine,
            "INSERT INTO routine_bound_visible.value_generated (source) VALUES (1) RETURNING v"
        ),
        Value::Int(9)
    );
    assert_eq!(
        scalar(&engine, "SELECT routine_bound_visible.atomic_probe() AS v"),
        Value::Int(9)
    );
    assert_eq!(
        sqlstate(&engine, "SELECT routine_bound_visible.invoker_probe() AS v"),
        "42501"
    );
    assert_eq!(
        scalar(&engine, "SELECT routine_bound_visible.definer_probe() AS v"),
        Value::Int(9)
    );
}
