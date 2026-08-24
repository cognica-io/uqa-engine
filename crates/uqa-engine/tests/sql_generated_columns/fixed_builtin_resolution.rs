//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common generated-column overload binding for fixed-signature built-ins.

use super::*;

#[test]
fn generated_fixed_builtin_bindings_preserve_user_selection_across_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("fixed-builtin-binding.uqa");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA fixed_generated",
            "CREATE FUNCTION fixed_generated.to_bin(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-bin'''",
            "CREATE FUNCTION fixed_generated.casefold(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user-fold'''",
            "CREATE FUNCTION fixed_generated.uuid_extract_version(value UUID) RETURNS SMALLINT LANGUAGE SQL IMMUTABLE AS 'SELECT 9::smallint'",
            "SET search_path = fixed_generated, pg_catalog, public",
            "CREATE TABLE fixed_generated.generated_values (
                 number INTEGER,
                 source TEXT,
                 identifier UUID,
                 user_bin TEXT GENERATED ALWAYS AS (to_bin(number)) STORED,
                 builtin_bin TEXT GENERATED ALWAYS AS (pg_catalog.to_bin(number)) STORED,
                 user_fold TEXT GENERATED ALWAYS AS (casefold(source)) STORED,
                 builtin_fold TEXT GENERATED ALWAYS AS (pg_catalog.casefold(source)) STORED,
                 user_version SMALLINT GENERATED ALWAYS AS (uuid_extract_version(identifier)) STORED,
                 builtin_version SMALLINT GENERATED ALWAYS AS (pg_catalog.uuid_extract_version(identifier)) STORED
             )",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
    }
    let engine = Engine::open(&database).unwrap();
    engine
        .sql("SET search_path = pg_catalog, public", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO fixed_generated.generated_values(number, source, identifier)
             VALUES (5, 'Straße', '00000000-0000-7000-8000-000000000000')",
            &[],
        )
        .unwrap();
    let row = engine
        .sql(
            "SELECT user_bin, builtin_bin, user_fold, builtin_fold, user_version, builtin_version
             FROM fixed_generated.generated_values",
            &[],
        )
        .unwrap()
        .rows
        .pop()
        .unwrap();
    assert_eq!(row["user_bin"], Value::Str("user-bin".into()));
    assert_eq!(row["builtin_bin"], Value::Str("101".into()));
    assert_eq!(row["user_fold"], Value::Str("user-fold".into()));
    assert_eq!(row["builtin_fold"], Value::Str("strasse".into()));
    assert_eq!(row["user_version"], Value::Int(9));
    assert_eq!(row["builtin_version"], Value::Int(7));
}

#[test]
fn generated_fixed_builtin_resolution_reports_signatures_before_volatility() {
    let engine = Engine::new();
    for (index, expression, sqlstate) in [
        (0, "random(1)", "42883"),
        (1, "random(NULL, NULL)", "42725"),
        (2, "gen_random_uuid(1)", "42883"),
        (3, "uuidv4(1)", "42883"),
        (4, "uuidv7(1)", "42883"),
        (5, "uuidv7(bad => interval '1 day')", "42883"),
    ] {
        let sql = format!(
            "CREATE TABLE invalid_volatile_signature_{index} (value TEXT GENERATED ALWAYS AS ({expression}::text) STORED)"
        );
        let error = engine.sql(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate), "{expression}: {error}");
    }
    for (index, expression) in [
        "random()::text",
        "random(1, 10)::text",
        "gen_random_uuid()::text",
        "uuidv4()::text",
        "uuidv7()::text",
        "uuidv7(shift => interval '1 day')::text",
    ]
    .into_iter()
    .enumerate()
    {
        let sql = format!(
            "CREATE TABLE volatile_fixed_builtin_{index} (value TEXT GENERATED ALWAYS AS ({expression}) STORED)"
        );
        let error = engine.sql(&sql, &[]).unwrap_err();
        assert!(
            error.to_string().contains("not immutable"),
            "{expression}: {error}"
        );
    }
}

#[test]
fn generated_fixed_builtin_binding_rejects_invalid_names_and_user_volatility() {
    let engine = Engine::new();
    for (index, expression, columns) in [
        (0, "to_bin(value => source)", "source INTEGER"),
        (1, "casefold(value => source)", "source TEXT"),
        (
            2,
            "uuid_extract_version(value => source)::text",
            "source UUID",
        ),
    ] {
        let sql = format!(
            "CREATE TABLE invalid_fixed_name_{index} ({columns}, value TEXT GENERATED ALWAYS AS ({expression}) STORED)"
        );
        let error = engine.sql(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{expression}: {error}");
    }
    for sql in [
        "CREATE SCHEMA volatile_fixed",
        "CREATE FUNCTION volatile_fixed.to_oct(value INTEGER) RETURNS TEXT LANGUAGE SQL VOLATILE AS 'SELECT ''user'''",
        "SET search_path = volatile_fixed, pg_catalog, public",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let error = engine
        .sql(
            "CREATE TABLE volatile_fixed.generated_values (
                 source INTEGER,
                 value TEXT GENERATED ALWAYS AS (to_oct(source)) STORED
             )",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("not immutable"), "{error}");
}
