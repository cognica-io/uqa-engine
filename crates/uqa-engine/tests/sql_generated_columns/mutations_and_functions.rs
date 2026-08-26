//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column mutation paths and stored function execution.

use super::*;

#[test]
fn generated_columns_are_recomputed_for_upsert_and_merge() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_mutations (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_mutations (id, source) VALUES (1, 3)",
            &[],
        )
        .unwrap();
    let upsert = engine
        .sql(
            "INSERT INTO generated_mutations (id, source) VALUES (1, 7) ON CONFLICT (id) DO UPDATE SET source = EXCLUDED.source RETURNING virtual_value, stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(int(&upsert.rows[0], "virtual_value"), 8);
    assert_eq!(int(&upsert.rows[0], "stored_value"), 14);

    engine
        .sql(
            "CREATE TABLE generated_source (id INTEGER PRIMARY KEY, source INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_source VALUES (1, 9), (2, 4)", &[])
        .unwrap();
    let merged = engine
        .sql(
            "MERGE INTO generated_mutations AS target USING generated_source AS incoming ON target.id = incoming.id WHEN MATCHED THEN UPDATE SET source = incoming.source WHEN NOT MATCHED THEN INSERT VALUES (incoming.id, incoming.source) RETURNING merge_action() AS action, new.virtual_value AS virtual_value, new.stored_value AS stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(merged.rows.len(), 2);
    assert_eq!(
        merged.column_types,
        [
            Some(ColumnType::Text),
            Some(ColumnType::Integer),
            Some(ColumnType::Integer),
        ]
    );
    let update = merged
        .rows
        .iter()
        .find(|row| row.get("action") == Some(&Value::Str("UPDATE".into())))
        .unwrap();
    assert_eq!(int(update, "virtual_value"), 10);
    assert_eq!(int(update, "stored_value"), 18);
    let insert = merged
        .rows
        .iter()
        .find(|row| row.get("action") == Some(&Value::Str("INSERT".into())))
        .unwrap();
    assert_eq!(int(insert, "virtual_value"), 5);
    assert_eq!(int(insert, "stored_value"), 8);
}

#[test]
fn immutable_user_functions_are_stored_only_generation_expressions() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION generated_twice(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT value * 2'",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE generated_with_function (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (generated_twice(source)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_with_function (source) VALUES (6)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql("SELECT derived FROM generated_with_function", &[])
        .unwrap();
    assert_eq!(int(&result.rows[0], "derived"), 12);

    let error = engine
        .sql(
            "CREATE TABLE virtual_with_function (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (generated_twice(source))
             )",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("user-defined function"), "{error}");
    assert!(!engine.has_table("virtual_with_function").unwrap());
}

#[test]
fn scalar_and_generated_calls_preserve_quoted_named_argument_case() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION generated_quoted_name(\"Items\" INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 1'",
            &[],
        )
        .unwrap();

    let direct = engine
        .sql("SELECT generated_quoted_name(\"Items\" => 4) AS value", &[])
        .unwrap();
    assert_eq!(int(&direct.rows[0], "value"), 5);

    engine
        .sql(
            "CREATE TABLE generated_quoted_values (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (
                     generated_quoted_name(\"Items\" => source)
                 ) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_quoted_values(source) VALUES (4)",
            &[],
        )
        .unwrap();
    let generated = engine
        .sql("SELECT derived FROM generated_quoted_values", &[])
        .unwrap();
    assert_eq!(int(&generated.rows[0], "derived"), 5);

    for sql in [
        "SELECT generated_quoted_name(items => 4)",
        "CREATE TABLE generated_wrong_case (
             source INTEGER,
             derived INTEGER GENERATED ALWAYS AS (
                 generated_quoted_name(items => source)
             ) STORED
         )",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
}

#[test]
fn generated_function_bindings_select_and_depend_on_exact_overloads() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("generated-function-bindings.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE FUNCTION generated_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer'''",
            "CREATE FUNCTION generated_pick(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''text'''",
            "CREATE FUNCTION generated_literal_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer'''",
            "CREATE FUNCTION generated_literal_pick(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''text'''",
            "CREATE TABLE generated_bound_call (id INTEGER PRIMARY KEY, source INTEGER, derived TEXT GENERATED ALWAYS AS (generated_pick(source)) STORED)",
            "CREATE TABLE generated_unknown_call (id INTEGER PRIMARY KEY, derived TEXT GENERATED ALWAYS AS (generated_literal_pick(NULL)) STORED)",
            "INSERT INTO generated_bound_call (id, source) VALUES (1, NULL), (2, 7)",
            "INSERT INTO generated_unknown_call (id) VALUES (1)",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
        let bound = engine
            .sql(
                "SELECT id, derived FROM generated_bound_call ORDER BY id",
                &[],
            )
            .unwrap();
        assert_eq!(bound.rows[0]["derived"], Value::Str("integer".into()));
        assert_eq!(bound.rows[1]["derived"], Value::Str("integer".into()));
        let unknown = engine
            .sql("SELECT derived FROM generated_unknown_call", &[])
            .unwrap();
        assert_eq!(unknown.rows[0]["derived"], Value::Str("text".into()));

        engine
            .sql("DROP FUNCTION generated_pick(TEXT)", &[])
            .unwrap();
        engine
            .sql("DROP FUNCTION generated_literal_pick(INTEGER)", &[])
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    for sql in [
        "DROP FUNCTION generated_pick(INTEGER)",
        "DROP FUNCTION generated_literal_pick(TEXT)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err().to_string();
        assert!(error.contains("generated column"), "{error}");
    }
    engine
        .sql(
            "CREATE OR REPLACE FUNCTION generated_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL VOLATILE AS 'SELECT ''replacement'''",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_bound_call (id, source) VALUES (3, 9)",
            &[],
        )
        .unwrap();
    let replacement = engine
        .sql("SELECT derived FROM generated_bound_call WHERE id = 3", &[])
        .unwrap();
    assert_eq!(
        replacement.rows[0]["derived"],
        Value::Str("replacement".into())
    );
}

#[test]
fn stored_generation_expression_is_evaluated_once_per_row_write() {
    let engine = Engine::new();
    for sql in [
        "CREATE SEQUENCE generated_evaluation_count START 1",
        "CREATE FUNCTION generated_counted(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT nextval(''generated_evaluation_count'')'",
        "CREATE TABLE generated_evaluation_rows (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (generated_counted(source)) STORED)",
        "INSERT INTO generated_evaluation_rows (id, source) VALUES (1, 10)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let inserted = engine
        .sql(
            "SELECT derived, currval('generated_evaluation_count') AS calls FROM generated_evaluation_rows",
            &[],
        )
        .unwrap();
    assert_eq!(int(&inserted.rows[0], "derived"), 1);
    assert_eq!(int(&inserted.rows[0], "calls"), 1);

    engine
        .sql(
            "UPDATE generated_evaluation_rows SET source = 20 WHERE id = 1",
            &[],
        )
        .unwrap();
    let updated = engine
        .sql(
            "SELECT derived, currval('generated_evaluation_count') AS calls FROM generated_evaluation_rows",
            &[],
        )
        .unwrap();
    assert_eq!(int(&updated.rows[0], "derived"), 2);
    assert_eq!(int(&updated.rows[0], "calls"), 2);

    engine
        .sql(
            "INSERT INTO generated_evaluation_rows (id, source) SELECT 2, 30",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "MERGE INTO generated_evaluation_rows AS target USING (VALUES (3, 40)) AS source(id, value) ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id, source) VALUES (source.id, source.value)",
            &[],
        )
        .unwrap();
    let inserted = engine
        .sql(
            "SELECT array_agg(derived ORDER BY id) AS derived, currval('generated_evaluation_count') AS calls FROM generated_evaluation_rows",
            &[],
        )
        .unwrap();
    assert_eq!(
        inserted.rows[0]["derived"],
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![Value::Int(2), Value::Int(3), Value::Int(4),])
                .unwrap()
        )
    );
    assert_eq!(int(&inserted.rows[0], "calls"), 4);
}

#[test]
fn virtual_generation_is_deferred_until_required_by_read_or_constraint() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_virtual_late (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (1 / source) VIRTUAL)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_virtual_late (id, source) VALUES (1, 0)",
            &[],
        )
        .unwrap();
    let source_only = engine
        .sql("SELECT source FROM generated_virtual_late", &[])
        .unwrap();
    assert_eq!(int(&source_only.rows[0], "source"), 0);
    let error = engine
        .sql("SELECT derived FROM generated_virtual_late", &[])
        .unwrap_err()
        .to_string();
    assert!(
        error.to_ascii_lowercase().contains("division by zero"),
        "{error}"
    );

    engine
        .sql(
            "CREATE TABLE generated_virtual_checked (source INTEGER, derived INTEGER GENERATED ALWAYS AS (source + 1) VIRTUAL, CHECK (derived > 0))",
            &[],
        )
        .unwrap();
    let error = engine
        .sql(
            "INSERT INTO generated_virtual_checked (source) VALUES (-2)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert_eq!(
        error.to_string(),
        "new row for relation \"generated_virtual_checked\" violates check constraint \"generated_virtual_checked_derived_check\""
    );
}
