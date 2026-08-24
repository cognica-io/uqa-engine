//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_failing_sqlstate(engine: &Engine, sql: &str, expected: &str) {
    let Err(error) = engine.sql(sql, &[]) else {
        panic!("expected `{sql}` to fail with SQLSTATE {expected}");
    };
    assert_eq!(error.sqlstate(), Some(expected), "{sql}: {error}");
}

fn assert_pg_proc_attributes(
    engine: &Engine,
    name: &str,
    kind: &str,
    argument_oids: &[i64],
    volatility: &str,
    strict: bool,
) {
    let result = engine
        .sql(
            &format!(
                "SELECT prokind, proargtypes, provolatile, proisstrict \
                 FROM pg_catalog.pg_proc WHERE proname = '{name}'"
            ),
            &[],
        )
        .unwrap();
    let expected_argument_oids =
        Value::List(argument_oids.iter().copied().map(Value::Int).collect());
    let matching = result
        .rows
        .iter()
        .filter(|row| {
            row["prokind"] == Value::Str(kind.into())
                && row["proargtypes"] == expected_argument_oids
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "expected one pg_proc row for {name}({argument_oids:?}) of kind {kind}, got {:?}",
        result.rows
    );
    assert_eq!(matching[0]["provolatile"], Value::Str(volatility.into()));
    assert_eq!(matching[0]["proisstrict"], Value::Bool(strict));
}

#[test]
fn alter_function_changes_only_the_exact_overload_and_preserves_its_body() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION alter_exact(value integer) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''integer-body'''",
        "CREATE FUNCTION alter_exact(value bigint) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''bigint-body'''",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    assert_eq!(
        scalar(&engine, "SELECT alter_exact(7) AS v"),
        Value::Str("integer-body".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT alter_exact(7::bigint) AS v"),
        Value::Str("bigint-body".into())
    );

    engine
        .sql("ALTER FUNCTION alter_exact(integer) IMMUTABLE STRICT", &[])
        .unwrap();

    assert_pg_proc_attributes(&engine, "alter_exact", "f", &[23], "i", true);
    assert_pg_proc_attributes(&engine, "alter_exact", "f", &[20], "v", false);
    assert_eq!(
        scalar(&engine, "SELECT alter_exact(7) AS v"),
        Value::Str("integer-body".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT alter_exact(7::bigint) AS v"),
        Value::Str("bigint-body".into())
    );
}

#[test]
fn omitted_and_explicit_empty_signatures_have_distinct_resolution_rules() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION alter_unique(value integer) RETURNS integer LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT $1'",
        "CREATE FUNCTION alter_zero() RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''zero-body'''",
        "CREATE FUNCTION alter_zero(value integer) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''integer-body'''",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql("ALTER FUNCTION alter_unique STABLE STRICT", &[])
        .unwrap();
    assert_pg_proc_attributes(&engine, "alter_unique", "f", &[23], "s", true);

    assert_failing_sqlstate(&engine, "ALTER FUNCTION alter_zero IMMUTABLE", "42725");
    engine
        .sql("ALTER FUNCTION alter_zero() IMMUTABLE STRICT", &[])
        .unwrap();
    assert_pg_proc_attributes(&engine, "alter_zero", "f", &[], "i", true);
    assert_pg_proc_attributes(&engine, "alter_zero", "f", &[23], "v", false);
    assert_eq!(
        scalar(&engine, "SELECT alter_zero() AS v"),
        Value::Str("zero-body".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT alter_zero(7) AS v"),
        Value::Str("integer-body".into())
    );
}

#[test]
fn alter_function_and_procedure_report_postgresql_kind_and_missing_errors() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE PROCEDURE alter_only_procedure(value integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION alter_only_function(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT $1'",
            &[],
        )
        .unwrap();

    assert_failing_sqlstate(
        &engine,
        "ALTER FUNCTION alter_only_procedure(integer) IMMUTABLE",
        "42809",
    );
    assert_failing_sqlstate(
        &engine,
        "ALTER PROCEDURE alter_only_function(integer) STABLE",
        "42809",
    );
    assert_failing_sqlstate(
        &engine,
        "ALTER FUNCTION alter_missing(integer) IMMUTABLE",
        "42883",
    );
    assert_failing_sqlstate(
        &engine,
        "ALTER PROCEDURE alter_missing(integer) STABLE",
        "42883",
    );
    assert_failing_sqlstate(
        &engine,
        "ALTER PROCEDURE alter_only_procedure(integer) STABLE",
        "42P13",
    );
    assert_pg_proc_attributes(&engine, "alter_only_procedure", "p", &[23], "v", false);
}

#[test]
fn alter_routine_resolves_across_kinds_but_procedure_attributes_are_invalid() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION alter_neutral(value integer) RETURNS text LANGUAGE SQL VOLATILE CALLED ON NULL INPUT AS 'SELECT ''function-body'''",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE PROCEDURE alter_neutral(value text) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$",
            &[],
        )
        .unwrap();

    assert_failing_sqlstate(&engine, "ALTER ROUTINE alter_neutral VOLATILE", "42725");
    engine
        .sql("ALTER ROUTINE alter_neutral(integer) IMMUTABLE STRICT", &[])
        .unwrap();
    assert_pg_proc_attributes(&engine, "alter_neutral", "f", &[23], "i", true);
    assert_eq!(
        scalar(&engine, "SELECT alter_neutral(7) AS v"),
        Value::Str("function-body".into())
    );

    // PostgreSQL resolves the kind-neutral identity first, then rejects function-only attributes on the procedure with 42P13.
    assert_failing_sqlstate(
        &engine,
        "ALTER ROUTINE alter_neutral(text) STABLE CALLED ON NULL INPUT",
        "42P13",
    );
    assert_pg_proc_attributes(&engine, "alter_neutral", "p", &[25], "v", false);
}

#[test]
fn altered_function_attributes_and_compiled_body_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("alter-routine.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE FUNCTION persistent_alter(value integer) RETURNS integer LANGUAGE SQL IMMUTABLE STRICT AS 'SELECT $1 + 5'",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(&engine, "SELECT persistent_alter(7) AS v"),
            Value::Int(12)
        );

        engine
            .sql(
                "ALTER FUNCTION persistent_alter(integer) STABLE CALLED ON NULL INPUT",
                &[],
            )
            .unwrap();
        assert_pg_proc_attributes(&engine, "persistent_alter", "f", &[23], "s", false);
        assert_eq!(
            scalar(&engine, "SELECT persistent_alter(7) AS v"),
            Value::Int(12)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    assert_pg_proc_attributes(&reopened, "persistent_alter", "f", &[23], "s", false);
    assert_eq!(
        scalar(&reopened, "SELECT persistent_alter(7) AS v"),
        Value::Int(12)
    );
}

#[test]
fn alter_identity_resolves_percent_type_and_search_path_shadowing_before_kind() {
    let engine = Engine::new();
    for ddl in [
        "CREATE TABLE alter_type_source(value bigint)",
        "CREATE FUNCTION alter_percent(value bigint) RETURNS bigint LANGUAGE SQL VOLATILE AS 'SELECT $1'",
        "CREATE SCHEMA alter_first",
        "CREATE SCHEMA alter_second",
        "CREATE PROCEDURE alter_first.shadowed(value integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$",
        "CREATE FUNCTION alter_second.shadowed(value integer) RETURNS integer LANGUAGE SQL VOLATILE AS 'SELECT $1'",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql(
            "ALTER FUNCTION alter_percent(alter_type_source.value%TYPE) IMMUTABLE STRICT",
            &[],
        )
        .unwrap();
    assert_pg_proc_attributes(&engine, "alter_percent", "f", &[20], "i", true);

    engine
        .sql("SET search_path TO alter_first, alter_second, public", &[])
        .unwrap();
    assert_failing_sqlstate(
        &engine,
        "ALTER FUNCTION shadowed(integer) IMMUTABLE",
        "42809",
    );
    assert_failing_sqlstate(&engine, "ALTER FUNCTION shadowed IMMUTABLE", "42883");
    assert_failing_sqlstate(
        &engine,
        "ALTER ROUTINE shadowed(integer) IMMUTABLE",
        "42P13",
    );
}
