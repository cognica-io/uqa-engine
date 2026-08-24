//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::ArrayValue;
use uqa_sql::ast::ColumnType;

fn assert_sqlstate(engine: &Engine, sql: &str, expected: &str) {
    let Err(error) = engine.sql(sql, &[]) else {
        panic!("expected `{sql}` to fail");
    };
    assert_eq!(error.sqlstate(), Some(expected), "{sql}: {error}");
}

#[test]
fn polymorphic_scalar_substitution_and_ambiguity_match_postgresql_18() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION poly_identity(value anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION poly_pair(value anyelement, items anyarray) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION compatible_pair(left_value anycompatible, right_value anycompatible) RETURNS anycompatible LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION shape(elem anyelement) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''element'''",
        "CREATE FUNCTION shape(arr anyarray) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''array'''",
    ] {
        engine.sql(ddl, &[]).unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let identity = engine
        .sql(
            "SELECT poly_identity(7::bigint) AS v, pg_typeof(poly_identity(7::bigint)) AS t",
            &[],
        )
        .unwrap();
    assert_eq!(identity.rows[0]["v"], Value::Int(7));
    assert_eq!(identity.rows[0]["t"], Value::Str("bigint".into()));
    assert_eq!(identity.column_types[0], Some(ColumnType::BigInteger));

    let compatible = engine
        .sql(
            "SELECT compatible_pair(1::smallint, 2::bigint) AS v, pg_typeof(compatible_pair(1::smallint, 2::bigint)) AS t",
            &[],
        )
        .unwrap();
    assert_eq!(compatible.rows[0]["v"], Value::Int(1));
    assert_eq!(compatible.rows[0]["t"], Value::Str("bigint".into()));
    assert_eq!(compatible.column_types[0], Some(ColumnType::BigInteger));

    assert_sqlstate(&engine, "SELECT poly_identity(NULL)", "42804");
    assert_sqlstate(&engine, "SELECT poly_pair(1, ARRAY['x'])", "42883");
    assert_sqlstate(&engine, "SELECT shape(ARRAY[1,2])", "42725");
    assert_eq!(
        scalar(&engine, "SELECT shape(elem => ARRAY[1,2]) AS v"),
        Value::Str("element".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT shape(arr => ARRAY[1,2]) AS v"),
        Value::Str("array".into())
    );
}

#[test]
fn implicit_and_explicit_variadic_calls_share_one_declared_array_identity() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION pack(VARIADIC items integer[]) RETURNS integer[] LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION choose(value integer) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''fixed'''",
        "CREATE FUNCTION choose(VARIADIC items integer[]) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''variadic'''",
    ] {
        engine.sql(ddl, &[]).unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let expected = Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap());
    assert_eq!(scalar(&engine, "SELECT pack(1, 2) AS v"), expected);
    assert_eq!(
        scalar(&engine, "SELECT pack(VARIADIC ARRAY[1,2]) AS v"),
        expected
    );
    assert_sqlstate(&engine, "SELECT pack()", "42883");
    assert_eq!(
        scalar(&engine, "SELECT choose(1) AS v"),
        Value::Str("fixed".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT choose(1, 2) AS v"),
        Value::Str("variadic".into())
    );

    assert_sqlstate(&engine, "DROP FUNCTION pack(integer, integer)", "42883");
    engine.sql("DROP FUNCTION pack(integer[])", &[]).unwrap();
}

#[test]
fn compatible_families_and_typed_routine_parameters_keep_concrete_call_types() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION compatible_value(left_value anycompatible, right_value anycompatible) RETURNS anycompatible LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION compatible_nonarray(left_value anycompatiblenonarray, right_value anycompatiblenonarray) RETURNS anycompatible LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION compatible_arrays(left_value anycompatiblearray, right_value anycompatiblearray) RETURNS anycompatiblearray LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION nested_kind(value integer) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer'''",
        "CREATE FUNCTION nested_kind(value bigint) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''bigint'''",
        "CREATE FUNCTION polymorphic_nested(value anyelement) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT nested_kind($1)'",
        "CREATE FUNCTION plpgsql_identity(value anyelement) RETURNS anyelement LANGUAGE plpgsql IMMUTABLE AS $$ BEGIN RETURN value; END $$",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let promoted = engine
        .sql(
            "SELECT compatible_value(1::smallint, 2::bigint) AS v, pg_typeof(compatible_value(1::smallint, 2::bigint)) AS t",
            &[],
        )
        .unwrap();
    assert_eq!(promoted.rows[0]["v"], Value::Int(1));
    assert_eq!(promoted.rows[0]["t"], Value::Str("bigint".into()));
    assert_eq!(promoted.column_types[0], Some(ColumnType::BigInteger));
    assert_eq!(
        scalar(
            &engine,
            "SELECT pg_typeof(compatible_nonarray(1::smallint, 2::bigint)) AS v"
        ),
        Value::Str("bigint".into())
    );
    assert_sqlstate(
        &engine,
        "SELECT compatible_nonarray(ARRAY[1], ARRAY[2])",
        "42883",
    );

    let unknown = engine
        .sql(
            "SELECT compatible_value('left', 'right') AS v, pg_typeof(compatible_value('left', 'right')) AS t",
            &[],
        )
        .unwrap();
    assert_eq!(unknown.rows[0]["v"], Value::Str("left".into()));
    assert_eq!(unknown.rows[0]["t"], Value::Str("text".into()));

    let arrays = engine
        .sql(
            "SELECT compatible_arrays(ARRAY[1]::integer[], ARRAY[2]::bigint[]) AS v, pg_typeof(compatible_arrays(ARRAY[1]::integer[], ARRAY[2]::bigint[])) AS t",
            &[],
        )
        .unwrap();
    assert_eq!(
        arrays.rows[0]["v"],
        Value::Array(ArrayValue::try_new(vec![Value::Int(1)]).unwrap())
    );
    assert_eq!(arrays.rows[0]["t"], Value::Str("bigint[]".into()));
    assert_eq!(
        scalar(&engine, "SELECT polymorphic_nested(7::bigint) AS v"),
        Value::Str("bigint".into())
    );
    let plpgsql = engine
        .sql(
            "SELECT plpgsql_identity(7::bigint) AS v, pg_typeof(plpgsql_identity(7::bigint)) AS t",
            &[],
        )
        .unwrap();
    assert_eq!(plpgsql.rows[0]["v"], Value::Int(7));
    assert_eq!(plpgsql.rows[0]["t"], Value::Str("bigint".into()));
}

#[test]
fn sql_polymorphic_variadic_table_and_setof_results_keep_concrete_types() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION poly_table(VARIADIC items anycompatiblearray) RETURNS TABLE(item anycompatible) LANGUAGE SQL IMMUTABLE AS 'SELECT unnest(items)'",
        "CREATE FUNCTION poly_set(VARIADIC items anyarray) RETURNS SETOF anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT unnest(items)'",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let table = engine
        .sql(
            "SELECT item, pg_typeof(item) AS item_type FROM poly_table(1::smallint, 2::bigint) ORDER BY item",
            &[],
        )
        .unwrap();
    assert_eq!(
        table
            .rows
            .iter()
            .map(|row| row["item"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
    assert_eq!(
        table
            .rows
            .iter()
            .map(|row| row["item_type"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("bigint".into()), Value::Str("bigint".into())]
    );
    assert_eq!(table.column_types[0], Some(ColumnType::BigInteger));

    let explicit_table = engine
        .sql(
            "SELECT item, pg_typeof(item) AS item_type FROM poly_table(VARIADIC items => ARRAY[3,4]::bigint[]) ORDER BY item",
            &[],
        )
        .unwrap();
    assert_eq!(
        explicit_table
            .rows
            .iter()
            .map(|row| row["item"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(3), Value::Int(4)]
    );
    assert_eq!(explicit_table.column_types[0], Some(ColumnType::BigInteger));

    let set = engine
        .sql(
            "SELECT value, pg_typeof(value) AS value_type FROM poly_set(VARIADIC ARRAY[9,10]::bigint[]) AS result(value) ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(
        set.rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(9), Value::Int(10)]
    );
    assert_eq!(
        set.rows
            .iter()
            .map(|row| row["value_type"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("bigint".into()), Value::Str("bigint".into())]
    );
    assert_eq!(set.column_types[0], Some(ColumnType::BigInteger));

    let projected_set = engine
        .sql(
            "SELECT value, pg_typeof(value) AS value_type FROM (SELECT poly_set(VARIADIC ARRAY[15,16]::bigint[]) AS value) expanded ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(
        projected_set
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(15), Value::Int(16)]
    );
    assert!(projected_set
        .rows
        .iter()
        .all(|row| row["value_type"] == Value::Str("bigint".into())));
    assert_eq!(projected_set.column_types[0], Some(ColumnType::BigInteger));
}

#[test]
fn plpgsql_polymorphic_variadic_return_next_keeps_concrete_set_type() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION pl_poly_set(VARIADIC items anyarray) RETURNS SETOF anyelement LANGUAGE plpgsql IMMUTABLE AS $$ BEGIN RETURN NEXT items[1]; RETURN NEXT items[2]; END $$",
            &[],
        )
        .unwrap();

    let implicit = engine
        .sql(
            "SELECT value, pg_typeof(value) AS value_type FROM pl_poly_set(11::bigint, 12::bigint) AS result(value) ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(
        implicit
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(11), Value::Int(12)]
    );
    assert_eq!(
        implicit
            .rows
            .iter()
            .map(|row| row["value_type"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("bigint".into()), Value::Str("bigint".into())]
    );
    assert_eq!(implicit.column_types[0], Some(ColumnType::BigInteger));

    let explicit = engine
        .sql(
            "SELECT value, pg_typeof(value) AS value_type FROM pl_poly_set(VARIADIC ARRAY[13,14]::integer[]) AS result(value) ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(
        explicit
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(13), Value::Int(14)]
    );
    assert_eq!(explicit.column_types[0], Some(ColumnType::Integer));
}

#[test]
fn variadic_procedure_call_supports_implicit_explicit_and_named_arrays() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE variadic_call_log(sequence SERIAL PRIMARY KEY, items integer[])",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE PROCEDURE record_items(VARIADIC items integer[]) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO variadic_call_log(items) VALUES (items); END $$",
            &[],
        )
        .unwrap();

    engine.sql("CALL record_items(1, 2)", &[]).unwrap();
    engine
        .sql("CALL record_items(VARIADIC ARRAY[3,4])", &[])
        .unwrap();
    engine
        .sql("CALL record_items(VARIADIC items => ARRAY[5,6])", &[])
        .unwrap();

    let result = engine
        .sql(
            "SELECT sequence, items, pg_typeof(items) AS items_type FROM variadic_call_log ORDER BY sequence",
            &[],
        )
        .unwrap();
    let arrays = result
        .rows
        .iter()
        .map(|row| row["items"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        arrays,
        vec![
            Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap()),
            Value::Array(ArrayValue::try_new(vec![Value::Int(3), Value::Int(4)]).unwrap()),
            Value::Array(ArrayValue::try_new(vec![Value::Int(5), Value::Int(6)]).unwrap()),
        ]
    );
    assert!(result
        .rows
        .iter()
        .all(|row| row["items_type"] == Value::Str("integer[]".into())));
    assert_eq!(
        result.column_types[1],
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );
}

#[test]
fn generated_expression_uses_concrete_polymorphic_return_for_nested_overload() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION generated_identity(value anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
        "CREATE FUNCTION generated_kind(value integer) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer'''",
        "CREATE FUNCTION generated_kind(value bigint) RETURNS text LANGUAGE SQL IMMUTABLE AS 'SELECT ''bigint'''",
        "CREATE TABLE generated_poly(source bigint, copied bigint GENERATED ALWAYS AS (generated_identity(source)) STORED, kind text GENERATED ALWAYS AS (generated_kind(generated_identity(source))) STORED)",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    engine
        .sql("INSERT INTO generated_poly(source) VALUES (42)", &[])
        .unwrap();

    let result = engine
        .sql(
            "SELECT source, copied, kind, pg_typeof(copied) AS copied_type FROM generated_poly",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["source"], Value::Int(42));
    assert_eq!(result.rows[0]["copied"], Value::Int(42));
    assert_eq!(result.rows[0]["kind"], Value::Str("bigint".into()));
    assert_eq!(result.rows[0]["copied_type"], Value::Str("bigint".into()));
    assert_eq!(result.column_types[1], Some(ColumnType::BigInteger));
}

#[test]
fn variadic_defaults_and_named_explicit_arrays_follow_postgresql_call_shape() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION variadic_default(VARIADIC items integer[] DEFAULT ARRAY[9]) RETURNS integer[] LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT variadic_default() AS v"),
        Value::Array(ArrayValue::try_new(vec![Value::Int(9)]).unwrap())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT variadic_default(VARIADIC items => ARRAY[1,2]) AS v",
        ),
        Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap())
    );
    assert_sqlstate(
        &engine,
        "SELECT variadic_default(items => ARRAY[1,2])",
        "42883",
    );
}

#[test]
fn procedure_out_slots_use_call_shape_but_not_routine_identity() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE PROCEDURE poly_out(IN x anyelement, OUT y anyelement) LANGUAGE plpgsql AS $$ BEGIN y := x; END $$",
            &[],
        )
        .unwrap();
    let result = engine.sql("CALL poly_out(7, NULL)", &[]).unwrap();
    assert_eq!(result.rows[0]["y"], Value::Int(7));
    assert_eq!(result.column_types, vec![Some(ColumnType::Integer)]);

    let duplicate = engine
        .sql(
            "CREATE FUNCTION poly_out(x anyelement) RETURNS anyelement LANGUAGE SQL AS 'SELECT $1'",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42723"), "{duplicate}");

    engine
        .sql("DROP PROCEDURE poly_out(anyelement)", &[])
        .unwrap();
}

#[test]
fn pseudo_type_and_variadic_declarations_fail_at_postgresql_validation_boundaries() {
    let engine = Engine::new();
    for (sql, state) in [
        (
            "CREATE FUNCTION bad_record(value record) RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
            "42P13",
        ),
        (
            "CREATE FUNCTION bad_void(value void) RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
            "42P13",
        ),
        (
            "CREATE FUNCTION bad_return() RETURNS anyelement LANGUAGE SQL AS 'SELECT NULL'",
            "42P13",
        ),
        (
            "CREATE FUNCTION bad_variadic(VARIADIC value integer) RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
            "42P13",
        ),
        (
            "CREATE FUNCTION bad_atomic(value anyelement) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 1; END",
            "42P13",
        ),
    ] {
        assert_sqlstate(&engine, sql, state);
    }
}

#[test]
fn concrete_polymorphic_view_binding_survives_reopen_and_keeps_exact_dependency() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("polymorphic-routine.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE FUNCTION persistent_identity(value anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW persistent_poly_view AS SELECT persistent_identity(7::bigint) AS v",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION persistent_poly_set(VARIADIC items anyarray) RETURNS SETOF anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT unnest(items)'",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW persistent_poly_set_view AS SELECT value FROM persistent_poly_set(VARIADIC ARRAY[8,9]::bigint[]) AS result(value)",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    let result = reopened
        .sql("SELECT v FROM persistent_poly_view", &[])
        .unwrap();
    assert_eq!(result.rows[0]["v"], Value::Int(7));
    assert_eq!(result.column_types, vec![Some(ColumnType::BigInteger)]);
    let set = reopened
        .sql(
            "SELECT value FROM persistent_poly_set_view ORDER BY value",
            &[],
        )
        .unwrap();
    assert_eq!(
        set.rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(8), Value::Int(9)]
    );
    assert_eq!(set.column_types, vec![Some(ColumnType::BigInteger)]);
    assert_sqlstate(
        &reopened,
        "DROP FUNCTION persistent_identity(bigint)",
        "42883",
    );
    assert_sqlstate(
        &reopened,
        "DROP FUNCTION persistent_identity(anyelement)",
        "2BP01",
    );
}

#[test]
fn drop_function_cascade_removes_exact_polymorphic_views_and_generated_columns() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("polymorphic-routine-cascade.db");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE FUNCTION cascade_identity(value anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
            "CREATE TABLE cascade_source(x integer, y integer GENERATED ALWAYS AS (cascade_identity(x)) STORED)",
            "CREATE VIEW cascade_direct AS SELECT cascade_identity(x) AS v FROM cascade_source",
            "CREATE VIEW cascade_nested AS SELECT v FROM cascade_direct",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        let restricted = engine
            .sql("DROP FUNCTION cascade_identity(anyelement)", &[])
            .unwrap_err();
        assert_eq!(restricted.sqlstate(), Some("2BP01"), "{restricted}");
        engine
            .sql("DROP FUNCTION cascade_identity(anyelement) CASCADE", &[])
            .unwrap();
        let columns = engine.describe_table("cascade_source").unwrap().unwrap();
        assert_eq!(
            columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["x"]
        );
        assert!(engine.sql("SELECT * FROM cascade_direct", &[]).is_err());
        assert!(engine.sql("SELECT * FROM cascade_nested", &[]).is_err());
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        reopened
            .describe_table("cascade_source")
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    let missing = reopened.sql("SELECT cascade_identity(1)", &[]).unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42883"), "{missing}");
}
