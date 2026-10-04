//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the attributes of `CREATE FUNCTION` and `ALTER FUNCTION`: repeated and procedure-only attributes, SET, COST, ROWS, SUPPORT, PARALLEL, the language, LEAKPROOF, transforms, argument types and defaults, the result type and the body, each checked at `PostgreSQL`'s stage, and the owner each routine command names.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn verify_routine_attributes(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/routine_attributes_oracle.expected.json"),
    );
}

#[test]
fn routine_attributes_match_postgresql_memory() {
    verify_routine_attributes(&Engine::new());
}

#[test]
fn routine_attributes_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_routine_attributes(
        &Engine::open(&directory.path().join("routine-attributes.db")).unwrap(),
    );
}

#[test]
fn routine_cost_and_rows_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("routine-cost.db");
    {
        let engine = Engine::open(&path).unwrap();
        for sql in [
            "CREATE FUNCTION costly() RETURNS SETOF int LANGUAGE sql COST 7 ROWS 42 AS 'select 1'",
            "CREATE FUNCTION plain() RETURNS int LANGUAGE sql AS 'select 1'",
            "ALTER FUNCTION plain() COST 3.5",
            // A float4 COST overflows to infinity, which the JSON catalog keeps as text.
            "CREATE FUNCTION endless() RETURNS int LANGUAGE sql COST 1e50 AS 'select 1'",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
    }
    let engine = Engine::open(&path).unwrap();
    let result = engine
        .sql(
            "SELECT proname, procost, prorows FROM pg_proc
             WHERE proname IN ('costly', 'endless', 'plain') ORDER BY proname",
            &[],
        )
        .unwrap();
    let rows = result
        .rows
        .iter()
        .map(|row| {
            (
                row["proname"].clone(),
                row["procost"].clone(),
                row["prorows"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (
                Value::Str("costly".into()),
                Value::Float(7.0),
                Value::Float(42.0)
            ),
            (
                Value::Str("endless".into()),
                Value::Float(f64::INFINITY),
                Value::Float(0.0)
            ),
            (
                Value::Str("plain".into()),
                Value::Float(3.5),
                Value::Float(0.0)
            ),
        ]
    );
}
