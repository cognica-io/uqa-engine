//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `pg_get_function_sqlbody`: `RETURN` bodies and `BEGIN ATOMIC` bodies whose `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `MERGE` and `WITH` statements print as `get_query_def` prints them, with parameters qualified by the routine's name once a statement has a range table.

use std::sync::Arc;
use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn verify_function_sqlbody(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/function_sqlbody_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn function_sqlbody_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sqlbody.db");
    let engine = open(provider, &path);
    verify_function_sqlbody(&engine);
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn routine_window_parameters_preserve_results_and_persistent_bodies(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("window-parameters.db");
    let engine = open(provider, &path);
    engine
        .sql(
            "CREATE TABLE items(id integer); INSERT INTO items VALUES (1), (2), (3)",
            &[],
        )
        .unwrap();
    for (name, window) in [
        ("partition_totals", "PARTITION BY n"),
        (
            "frame_totals",
            "ORDER BY id ROWS BETWEEN n PRECEDING AND CURRENT ROW",
        ),
    ] {
        for source in [false, true] {
            let name = format!("{name}_{}", if source { "source" } else { "standard" });
            let query = format!("SELECT sum(id) OVER ({window}) FROM items ORDER BY id");
            let body = if source {
                format!("AS $body${query}$body$")
            } else {
                format!("BEGIN ATOMIC {query}; END")
            };
            engine.sql(&format!("CREATE FUNCTION {name}(n integer) RETURNS SETOF bigint LANGUAGE SQL {body}"), &[]).unwrap();
        }
    }
    // PostgreSQL 18.4 returns these same rows for both body forms and successive argument values.
    let verify = |engine: &Engine| {
        for (name, parameter, expected) in [
            ("partition_totals", 2, [6, 6, 6]),
            ("frame_totals", 1, [1, 3, 5]),
            ("frame_totals", 0, [1, 2, 3]),
        ] {
            for body in ["source", "standard"] {
                let sql = format!("SELECT v FROM {name}_{body}({parameter}) t(v)");
                let result = engine
                    .sql(&sql, &[])
                    .unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(result.columns, ["v"], "{sql}");
                let actual = result
                    .rows
                    .iter()
                    .map(|row| row["v"].clone())
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected.map(uqa_core::Value::Int), "{sql}");
            }
        }
    };
    verify(&engine);
    drop(engine);
    if provider != 0 {
        verify(&open(provider, &path));
    }
}
