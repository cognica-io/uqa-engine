//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `min` and `max` reach `PostgreSQL` 18's overloads by implicit coercion: OID alias types reach `min(oid)`, the vector types match `anyarray` and keep their own type, and a domain reaches its base type's overload. A type without a reachable overload is 42883.

use uqa_core::Value;
use uqa_engine::Engine;

fn text(engine: &Engine, sql: &str) -> Vec<String> {
    let result = engine.sql(sql, &[]).unwrap();
    (0..result.columns.len())
        .map(|column| match result.value_at(0, column) {
            Some(Value::Str(text)) => text.clone(),
            other => panic!("{sql} returned {other:?}"),
        })
        .collect()
}

#[test]
fn implicit_coercions_select_postgresql_overloads() {
    let engine = Engine::new();
    engine
        .sql("CREATE DOMAIN small_count AS smallint", &[])
        .unwrap();
    for (sql, expected) in [
        (
            "SELECT pg_typeof(min(v))::text AS t, min(v)::text AS v FROM (VALUES ('1 2'::int2vector), ('1 1'::int2vector)) AS s(v)",
            ["int2vector", "1 1"],
        ),
        (
            "SELECT pg_typeof(max(v))::text AS t, max(v)::text AS v FROM (VALUES ('3 4'::oidvector), ('5'::oidvector)) AS s(v)",
            ["oidvector", "5"],
        ),
        (
            "SELECT pg_typeof(max(v))::text AS t, (max(v) = 'pg_class'::regclass::oid)::text AS v FROM (VALUES ('pg_class'::regclass), ('pg_type'::regclass)) AS s(v)",
            ["oid", "true"],
        ),
        (
            "SELECT pg_typeof(min(v))::text AS t, min(v)::text AS v FROM (VALUES ('b'::name), ('a'::name)) AS s(v)",
            ["text", "a"],
        ),
        (
            "SELECT pg_typeof(min(v))::text AS t, min(v)::text AS v FROM (VALUES (2::small_count), (1::small_count)) AS s(v)",
            ["smallint", "1"],
        ),
    ] {
        assert_eq!(text(&engine, sql), expected, "{sql}");
    }
}

#[test]
fn types_without_a_reachable_overload_are_rejected() {
    let engine = Engine::new();
    for (sql, message) in [
        ("SELECT min(true)", "function min(boolean) does not exist"),
        (
            "SELECT max('00000000-0000-0000-0000-000000000000'::uuid)",
            "function max(uuid) does not exist",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
}
