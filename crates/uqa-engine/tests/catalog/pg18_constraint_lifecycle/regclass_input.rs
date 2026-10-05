//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `regclassin` at analysis: a `regclass` or `regclass[]` literal in a column default, generation expression, CHECK constraint, domain default, view or prepared statement is read when the statement is analyzed, so a relation the name does not find reports `42P01` there, stored constants carry the relation's OID and print by its visible name after a rename, prepared statements resolve their written names again at `EXECUTE`, `-` and digit strings are OIDs with `oidin`'s `22P02` and `22003`, names `SplitIdentifierString` rejects report `42602`, lookup failures name the parsed components, `to_regclass` returns NULL for those inputs, and arrays of OID alias types read each element.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn verify_regclass_input(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/regclass_input_oracle.expected.json"),
    );
}

#[test]
fn regclass_input_matches_postgresql_memory() {
    verify_regclass_input(&Engine::new());
}

#[test]
fn regclass_input_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_regclass_input(&Engine::open(&directory.path().join("regclass-input.db")).unwrap());
}

fn text_at(engine: &Engine, sql: &str, column: usize) -> String {
    match engine.sql(sql, &[]).unwrap().value_at(0, column) {
        Some(Value::Str(text)) => text.clone(),
        other => panic!("{sql}: {other:?}"),
    }
}

/// A view stores its `regclass` constants as relation OIDs, so a reopened database prints and resolves them by the relation's current name.
#[test]
fn view_regclass_constants_survive_reopening() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("regclass-view.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("CREATE TABLE rcv_t (a int)", &[]).unwrap();
        engine
            .sql(
                "CREATE VIEW rcv_v AS SELECT 'rcv_t'::regclass AS r, '{rcv_t}'::regclass[] AS rs, 'rcv_t'::regclass::text AS rt",
                &[],
            )
            .unwrap();
        engine
            .sql("ALTER TABLE rcv_t RENAME TO rcv_t2", &[])
            .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    let select = "SELECT r::text, rs::text, rt FROM rcv_v";
    assert_eq!(text_at(&engine, select, 0), "rcv_t2");
    assert_eq!(text_at(&engine, select, 1), "{rcv_t2}");
    assert_eq!(text_at(&engine, select, 2), "rcv_t2");
    assert_eq!(
        text_at(
            &engine,
            "SELECT pg_get_viewdef('rcv_v'::regclass, true)",
            0
        ),
        " SELECT 'rcv_t2'::regclass AS r,\n    '{rcv_t2}'::regclass[] AS rs,\n    'rcv_t2'::regclass::text AS rt;"
    );
}
