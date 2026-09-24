//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cascading column changes exclude statistics publication until transaction or savepoint release.

use super::{after_wait, peer_lock, sessions, sql, Engine, Value};

fn fixture(engine: &Engine, kind: &str) -> &'static str {
    let (create, columns, drop) = match kind {
        "sequence" => (
            "CREATE SEQUENCE dependency_source",
            "a regclass GENERATED ALWAYS AS ('dependency_source'::regclass) STORED, b regclass GENERATED ALWAYS AS ('dependency_source'::regclass) VIRTUAL, x integer, d regclass DEFAULT 'dependency_source'::regclass CHECK ('dependency_source'::regclass IS NOT NULL)",
            "DROP SEQUENCE dependency_source CASCADE",
        ),
        "function" | "schema" => (
            "CREATE SCHEMA dependency; CREATE FUNCTION dependency.source(v integer) RETURNS integer IMMUTABLE LANGUAGE SQL RETURN v+1",
            "a integer GENERATED ALWAYS AS (dependency.source(1)) STORED, b integer GENERATED ALWAYS AS (dependency.source(2)) VIRTUAL, x integer, d integer DEFAULT dependency.source(3) CHECK (dependency.source(4)>0)",
            if kind == "schema" {
                "DROP SCHEMA dependency CASCADE"
            } else {
                "DROP FUNCTION dependency.source(integer) CASCADE"
            },
        ),
        "domain" => (
            "CREATE DOMAIN dependency_source AS integer",
            "a dependency_source, b dependency_source, x integer, d integer",
            "DROP DOMAIN dependency_source CASCADE",
        ),
        _ => unreachable!(),
    };
    sql(engine, create);
    sql(
        engine,
        &format!("CREATE TABLE dependent({columns}); INSERT INTO dependent(x) VALUES(42)"),
    );
    drop
}

fn assert_columns(engine: &Engine, expected: &[&str]) {
    let rows = sql(engine, "SELECT column_name FROM information_schema.columns WHERE table_name='dependent' ORDER BY ordinal_position");
    assert_eq!(
        rows.rows
            .iter()
            .map(|row| row["column_name"].clone())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|name| Value::Str((*name).into()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        sql(engine, "SELECT x FROM dependent").rows[0]["x"],
        Value::Int(42)
    );
}

#[rstest::rstest]
fn cascading_columns_retain_exclusive_locks_until_savepoint_undo(
    #[values(0, 1, 2)] provider: usize,
    #[values("sequence", "function", "schema", "domain")] kind: &str,
) {
    let (_directory, first, second) = sessions(provider);
    let drop = fixture(&first, kind);
    sql(&first, &format!("BEGIN; SAVEPOINT kept; {drop}"));
    assert_columns(&first, &["x", "d"]);
    peer_lock(&second, "dependent", "ACCESS SHARE", false);
    peer_lock(&second, "dependent", "SHARE UPDATE EXCLUSIVE", false);
    sql(&first, "ROLLBACK TO kept");
    assert_columns(&first, &["a", "b", "x", "d"]);
    peer_lock(&second, "dependent", "SHARE UPDATE EXCLUSIVE", true);
    sql(&first, "COMMIT");
}

#[rstest::rstest]
fn cascading_columns_wait_for_statistics_before_staging_removal(
    #[values(0, 1, 2)] provider: usize,
    #[values("sequence", "function", "schema", "domain")] kind: &str,
) {
    let (_directory, first, second) = sessions(provider);
    let drop = fixture(&first, kind);
    sql(
        &first,
        "BEGIN; LOCK TABLE dependent IN SHARE UPDATE EXCLUSIVE MODE",
    );
    sql(&second, "BEGIN");
    let (second, result) = after_wait(
        &first,
        second,
        drop,
        "public.dependent",
        "ANALYZE dependent; COMMIT",
    );
    result.unwrap();
    assert_columns(&second, &["x", "d"]);
    sql(&second, "COMMIT");
    assert_columns(&first, &["x", "d"]);
}
