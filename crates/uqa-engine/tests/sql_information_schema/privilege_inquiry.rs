//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn error(engine: &Engine, sql: &str, sqlstate: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
}

#[test]
fn public_inquiry_subjects_apply_relation_and_exact_column_grants() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE inquiry_items(a integer, b integer); CREATE VIEW inquiry_view AS SELECT a, b FROM inquiry_items; CREATE MATERIALIZED VIEW inquiry_materialized AS SELECT a, b FROM inquiry_items; CREATE TABLE inquiry_columns(a integer, b integer); CREATE SEQUENCE inquiry_ids; GRANT SELECT ON inquiry_items, inquiry_view, inquiry_materialized, inquiry_ids TO PUBLIC; GRANT SELECT(a) ON inquiry_columns TO PUBLIC", &[]).unwrap();
    for subject in ["'public'", "0::oid", "4294967295::oid"] {
        for (relation, column) in [
            ("inquiry_items", "a"),
            ("inquiry_view", "a"),
            ("inquiry_materialized", "a"),
            ("inquiry_ids", "last_value"),
            ("pg_catalog.pg_class", "relname"),
        ] {
            for target in [
                format!("'{relation}'"),
                format!("'{relation}'::regclass::oid"),
            ] {
                for (privilege, expected) in [
                    ("SELECT", true),
                    ("INSERT", false),
                    ("SELECT WITH GRANT OPTION", false),
                ] {
                    let sql =
                        format!("SELECT has_table_privilege({subject}, {target}, '{privilege}')");
                    assert_eq!(scalar(&engine, &sql), Value::Bool(expected), "{sql}");
                    for column in [format!("'{column}'"), "1::smallint".into()] {
                        let sql = format!("SELECT has_column_privilege({subject}, {target}, {column}, '{privilege}')");
                        assert_eq!(scalar(&engine, &sql), Value::Bool(expected), "{sql}");
                    }
                }
            }
        }
        let sql = format!("SELECT has_table_privilege({subject}, 'inquiry_columns', 'SELECT')");
        assert_eq!(scalar(&engine, &sql), Value::Bool(false), "{sql}");
        for (column, expected) in [("a", true), ("b", false)] {
            let sql = format!(
                "SELECT has_column_privilege({subject}, 'inquiry_columns', '{column}', 'SELECT')"
            );
            assert_eq!(scalar(&engine, &sql), Value::Bool(expected), "{sql}");
        }
    }
    error(
        &engine,
        "SELECT has_table_privilege('PUBLIC', 'inquiry_items', 'SELECT')",
        "42704",
    );
    error(
        &engine,
        "SELECT has_column_privilege('PUBLIC', 'inquiry_items', 'a', 'SELECT')",
        "42704",
    );
    engine
        .sql(
            "CREATE ROLE \"PUBLIC\"; GRANT UPDATE ON inquiry_items TO \"PUBLIC\"",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('PUBLIC', 'inquiry_items', 'UPDATE')"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('public', 'inquiry_items', 'UPDATE')"
        ),
        Value::Bool(false)
    );
}

#[test]
fn relation_privilege_inquiry_preserves_name_oid_and_column_error_order() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE inquiry_reader; CREATE TABLE inquiry_items(a integer); CREATE SEQUENCE inquiry_ids", &[]).unwrap();
    for subject in ["", "'inquiry_reader', ", "0::oid, "] {
        for (target, expected) in [("'missing'", "42P01"), ("0::oid", "22023")] {
            error(
                &engine,
                &format!("SELECT has_table_privilege({subject}{target}, 'bad')"),
                expected,
            );
        }
        for (target, column, expected) in [
            ("'inquiry_items'", "'missing'", "42703"),
            ("'inquiry_items'", "0::smallint", "22023"),
            ("'inquiry_ids'", "0::smallint", "22023"),
            ("0::oid", "'missing'", "22023"),
            ("0::oid", "0::smallint", "22023"),
            ("'missing'", "'missing'", "42P01"),
            ("'missing'", "0::smallint", "42P01"),
        ] {
            error(
                &engine,
                &format!("SELECT has_column_privilege({subject}{target}, {column}, 'bad')"),
                expected,
            );
        }
    }
    error(
        &engine,
        "SELECT has_table_privilege('missing_role', 0::oid, 'bad')",
        "42704",
    );
    error(
        &engine,
        "SELECT has_column_privilege('missing_role', 0::oid, 0::smallint, 'bad')",
        "42704",
    );
    for sql in [
        "SELECT has_table_privilege(NULL::name, 'missing', 'bad')",
        "SELECT has_column_privilege(NULL::name, 'missing', 'missing', 'bad')",
        "SELECT has_table_privilege(0::oid, 'SELECT')",
        "SELECT has_column_privilege(0::oid, 'missing', 'SELECT')",
        "SELECT has_column_privilege('inquiry_items', 0::smallint, 'SELECT')",
        "SELECT has_column_privilege('inquiry_ids', 0::smallint, 'SELECT')",
    ] {
        assert_eq!(scalar(&engine, sql), Value::Null, "{sql}");
    }
}
