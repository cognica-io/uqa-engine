//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the `regtype`, `regproc`, `regprocedure` and `regnamespace` input functions at analysis: a literal of one of these types, or of an array of one, in a column default, generation expression, CHECK constraint, domain default, view or prepared statement is read when the statement is analyzed, so an object the name does not find reports the input function's error there, the stored constant is the object's OID, which `pg_get_expr`, `pg_get_constraintdef` and `pg_get_viewdef` print with the output function's canonical name after a rename and which `DROP` protects as a dependency, a prepared statement keeps the OID so that `EXECUTE` shows a renamed function or type, and a statement that runs at once checks every `reg*` literal before it runs, even one its rows never evaluate.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn verify_oid_alias_input(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/oid_alias_input_oracle.expected.json"),
    );
}

#[test]
fn oid_alias_input_matches_postgresql_memory() {
    verify_oid_alias_input(&Engine::new());
}

#[test]
fn oid_alias_input_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_oid_alias_input(&Engine::open(&directory.path().join("oid-alias-input.db")).unwrap());
}

fn text_at(engine: &Engine, sql: &str, column: usize) -> String {
    match engine.sql(sql, &[]).unwrap().value_at(0, column) {
        Some(Value::Str(text)) => text.clone(),
        other => panic!("{sql}: {other:?}"),
    }
}

/// Defaults and views store `regtype`, `regproc` and `regnamespace` constants as OIDs, so a reopened database prints and resolves them by the objects' current names.
#[test]
fn oid_alias_constants_survive_reopening() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("oid-alias-reopen.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql("CREATE TYPE oa_enum AS ENUM ('a')", &[])
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION oa_fn(int) RETURNS int LANGUAGE sql AS 'SELECT $1'",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE TABLE oa_t (a regtype DEFAULT 'oa_enum', b regproc DEFAULT 'oa_fn', c regprocedure DEFAULT 'oa_fn(int)', d regnamespace DEFAULT 'public', e regtype[] DEFAULT '{oa_enum,int4}')",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW oa_v AS SELECT 'oa_enum'::regtype AS t, 'oa_fn(int)'::regprocedure AS p",
                &[],
            )
            .unwrap();
        engine
            .sql("ALTER TYPE oa_enum RENAME TO oa_enum2", &[])
            .unwrap();
        engine
            .sql("ALTER FUNCTION oa_fn(int) RENAME TO oa_fn2", &[])
            .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    engine.sql("INSERT INTO oa_t DEFAULT VALUES", &[]).unwrap();
    let row = "SELECT a::text, b::text, c::text, d::text, e::text FROM oa_t";
    assert_eq!(text_at(&engine, row, 0), "oa_enum2");
    assert_eq!(text_at(&engine, row, 1), "oa_fn2");
    assert_eq!(text_at(&engine, row, 2), "oa_fn2(integer)");
    assert_eq!(text_at(&engine, row, 3), "public");
    assert_eq!(text_at(&engine, row, 4), "{oa_enum2,integer}");
    let defaults = "SELECT string_agg(pg_get_expr(adbin, adrelid), ', ' ORDER BY adnum) FROM pg_attrdef WHERE adrelid = 'oa_t'::regclass";
    assert_eq!(
        text_at(&engine, defaults, 0),
        "'oa_enum2'::regtype, 'oa_fn2'::regproc, 'oa_fn2(integer)'::regprocedure, 'public'::regnamespace, '{oa_enum2,integer}'::regtype[]"
    );
    let view = "SELECT t::text, p::text FROM oa_v";
    assert_eq!(text_at(&engine, view, 0), "oa_enum2");
    assert_eq!(text_at(&engine, view, 1), "oa_fn2(integer)");
    assert_eq!(
        text_at(&engine, "SELECT pg_get_viewdef('oa_v'::regclass, true)", 0),
        " SELECT 'oa_enum2'::regtype AS t,\n    'oa_fn2(integer)'::regprocedure AS p;"
    );
}
