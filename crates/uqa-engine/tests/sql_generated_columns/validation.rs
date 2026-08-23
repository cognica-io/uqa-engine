//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column definition validation and failure atomicity.

use super::*;

#[test]
fn generated_column_validation_is_failure_atomic_and_rejects_virtual_indexes() {
    let engine = Engine::new();
    for (table, sql, expected) in [
        (
            "volatile_generated",
            "CREATE TABLE volatile_generated (source INTEGER, derived DOUBLE PRECISION GENERATED ALWAYS AS (random()))",
            "not immutable",
        ),
        (
            "volatile_range_generated",
            "CREATE TABLE volatile_range_generated (source INTEGER, derived INTEGER GENERATED ALWAYS AS (random(1, 10)))",
            "not immutable",
        ),
        (
            "chained_generated",
            "CREATE TABLE chained_generated (source INTEGER, first_value INTEGER GENERATED ALWAYS AS (source + 1), second_value INTEGER GENERATED ALWAYS AS (first_value + 1))",
            "cannot use generated column",
        ),
        (
            "unknown_function_generated",
            "CREATE TABLE unknown_function_generated (source INTEGER, derived INTEGER GENERATED ALWAYS AS (missing_function(source)))",
            "unknown function",
        ),
        (
            "keyed_virtual_generated",
            "CREATE TABLE keyed_virtual_generated (source INTEGER, derived INTEGER GENERATED ALWAYS AS (source + 1) PRIMARY KEY)",
            "primary keys on virtual generated columns",
        ),
    ] {
        let error = match engine.sql(sql, &[]) {
            Ok(_) => panic!("{table} unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };
        assert!(error.to_ascii_lowercase().contains(expected), "{error}");
        assert!(!engine.has_table(table).unwrap());
    }

    engine
        .sql(
            "CREATE TABLE generated_indexes (
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source + 2) STORED
             )",
            &[],
        )
        .unwrap();
    let error = engine
        .sql(
            "CREATE INDEX generated_virtual_idx ON generated_indexes (virtual_value)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("virtual generated"), "{error}");
    assert!(engine.list_catalog_indexes().unwrap().is_empty());
    engine
        .sql(
            "CREATE INDEX generated_stored_idx ON generated_indexes (stored_value)",
            &[],
        )
        .unwrap();
}

#[test]
fn generated_expression_types_are_resolved_before_catalog_mutation() {
    let engine = Engine::new();
    for (table, sql) in [
        (
            "generated_boolean_mismatch",
            "CREATE TABLE generated_boolean_mismatch (source INTEGER, derived BOOLEAN GENERATED ALWAYS AS (source + 1) STORED)",
        ),
        (
            "generated_invalid_literal",
            "CREATE TABLE generated_invalid_literal (derived INTEGER GENERATED ALWAYS AS ('not-an-integer') STORED)",
        ),
        (
            "generated_invalid_function_argument",
            "CREATE TABLE generated_invalid_function_argument (source INTEGER, derived TEXT GENERATED ALWAYS AS (lower(source)) STORED)",
        ),
        (
            "generated_text_to_integer",
            "CREATE TABLE generated_text_to_integer (source TEXT, derived INTEGER GENERATED ALWAYS AS (source) STORED)",
        ),
        (
            "generated_unknown_operator",
            "CREATE TABLE generated_unknown_operator (derived INTEGER GENERATED ALWAYS AS ('1' + '2') STORED)",
        ),
        (
            "generated_unknown_common_type",
            "CREATE TABLE generated_unknown_common_type (derived INTEGER GENERATED ALWAYS AS (coalesce('1', '2')) STORED)",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err().to_string();
        assert!(!error.is_empty());
        assert!(!engine.has_table(table).unwrap(), "{table}: {error}");
    }

    engine
        .sql(
            "CREATE TABLE generated_typed_values (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 source_as_text TEXT GENERATED ALWAYS AS (source) STORED,
                 literal_integer INTEGER GENERATED ALWAYS AS ('1') STORED,
                 literal_boolean BOOLEAN GENERATED ALWAYS AS ('true') STORED,
                 lowered TEXT GENERATED ALWAYS AS (lower(source::text)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_typed_values (id, source) VALUES (1, 42)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT source_as_text, literal_integer, literal_boolean, lowered FROM generated_typed_values",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["source_as_text"], Value::Str("42".into()));
    assert_eq!(result.rows[0]["literal_integer"], Value::Int(1));
    assert_eq!(result.rows[0]["literal_boolean"], Value::Bool(true));
    assert_eq!(result.rows[0]["lowered"], Value::Str("42".into()));
}
