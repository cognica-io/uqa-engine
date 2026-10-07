//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nested command results independently checked against `PostgreSQL` 18.4 in Docker.

use std::{path::Path, sync::Arc};

use rstest::rstest;
use uqa_core::Value;
use uqa_engine::Engine;

fn open(path: &Path, backend: &str) -> Engine {
    match backend {
        "memory" => Engine::new(),
        "native" => Engine::open(path).unwrap(),
        "kv" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest]
fn indexed_nested_commands_find_staged_insert_rows(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[values("UPDATE t SET v = v + 1000", "DELETE FROM t")] command: &str,
    #[values("v = k", "v BETWEEN k AND k", "v IS NULL")] predicate: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("nested.db"), backend);
    engine
        .sql(
            "CREATE TABLE t (id integer PRIMARY KEY, v integer); CREATE INDEX t_v ON t(v)",
            &[],
        )
        .unwrap();
    engine.sql(&format!("CREATE FUNCTION mark(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ {command} WHERE {predicate} RETURNING id $$"), &[]).unwrap();
    let first = if predicate == "v IS NULL" {
        "NULL"
    } else {
        "g"
    };
    let result = engine.sql(&format!("INSERT INTO t SELECT g, CASE WHEN g = 2 THEN coalesce(mark(1), 0) ELSE {first} END FROM generate_series(1, 2) AS g RETURNING id, v"), &[]).unwrap();
    assert_eq!(result.affected_rows, 2);
    assert_eq!(
        result.rows[1]["v"],
        Value::Int(1),
        "{backend}: {command} WHERE {predicate}"
    );
}

fn pairs(engine: &Engine, sql: &str) -> Vec<(i64, i64)> {
    let mut rows = engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            let (Value::Int(id), Value::Int(value)) = (&row["id"], &row["v"]) else {
                panic!("{row:?}");
            };
            (*id, *value)
        })
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows
}

fn seed(engine: &Engine, indexed: bool) {
    engine.sql("CREATE TABLE t (id integer PRIMARY KEY, v integer); CREATE FUNCTION bump(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ UPDATE t SET v=v+1000 WHERE v=k RETURNING id $$", &[]).unwrap();
    if indexed {
        engine.sql("CREATE INDEX t_v ON t(v)", &[]).unwrap();
    }
}

#[rstest]
fn insert_preserves_nested_published_versions(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[values(false, true)] indexed: bool,
    #[values("SELECT g, CASE WHEN g=2 THEN coalesce(bump(1),0) ELSE g END FROM generate_series(1,3) AS g", "VALUES (1,1),(2,coalesce(bump(1),0)),(3,3)")]
    input: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("versions.db");
    let engine = open(&path, backend);
    seed(&engine, indexed);
    assert_eq!(
        pairs(&engine, &format!("INSERT INTO t {input} RETURNING id,v")),
        [(1, 1), (2, 1), (3, 3)]
    );
    assert_eq!(
        pairs(&engine, "SELECT id,v FROM t"),
        [(1, 1001), (2, 1), (3, 3)]
    );
    if backend != "memory" {
        engine.close().unwrap();
        drop(engine);
        let engine = open(&path, backend);
        assert_eq!(
            pairs(&engine, "SELECT id,v FROM t WHERE v=1001"),
            [(1, 1001)]
        );
        assert_eq!(pairs(&engine, "SELECT id,v FROM t WHERE v=1"), [(2, 1)]);
    }
}

#[rstest]
#[case::repeated("", "(1,1),(2,coalesce(bump(1),0)),(3,coalesce(bump(1001),0))", vec![(1,2001),(2,1),(3,1)])]
#[case::point_command("CREATE FUNCTION act(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ UPDATE t SET v=v+1000 WHERE id=k; SELECT k $$", "(1,1),(2,act(1)),(3,act(1))", vec![(1,2001),(2,1),(3,1)])]
#[case::delete("CREATE FUNCTION act(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ DELETE FROM t WHERE v=k RETURNING id $$", "(1,1),(2,coalesce(act(1),0)),(3,coalesce(bump(1),0))", vec![(2,1001),(3,2)])]
#[case::move_key("CREATE FUNCTION act(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ UPDATE t SET id=id+10 WHERE v=k RETURNING id $$", "(1,1),(2,coalesce(act(1),0)),(3,coalesce(bump(1),0))", vec![(2,11),(3,11),(11,1001)])]
#[case::undo("CREATE FUNCTION act(k integer) RETURNS integer VOLATILE LANGUAGE plpgsql AS $$ BEGIN UPDATE t SET v=9000 WHERE v=k; RAISE EXCEPTION 'undo'; EXCEPTION WHEN raise_exception THEN RETURN -1; END $$", "(1,1),(2,act(1)),(3,coalesce(bump(1),0))", vec![(1,1001),(2,-1),(3,1)])]
fn nested_changes_and_undo_preserve_later_key_reads(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[case] routine: &str,
    #[case] values: &str,
    #[case] expected: Vec<(i64, i64)>,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("changes.db"), backend);
    seed(&engine, true);
    if !routine.is_empty() {
        engine.sql(routine, &[]).unwrap();
    }
    engine.sql("BEGIN; SAVEPOINT before_insert", &[]).unwrap();
    engine
        .sql(&format!("INSERT INTO t VALUES {values}"), &[])
        .unwrap();
    assert_eq!(pairs(&engine, "SELECT id,v FROM t"), expected);
    engine.sql("ROLLBACK TO before_insert", &[]).unwrap();
    assert_eq!(pairs(&engine, "SELECT id,v FROM t").len(), 0);
    engine.sql("COMMIT", &[]).unwrap();
}

#[rstest]
fn update_preserves_nested_published_versions(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[values(
        "UPDATE t SET v=CASE WHEN id=2 THEN coalesce(bump(1001),0) ELSE v+1000 END RETURNING id,v",
        "UPDATE t SET v=CASE WHEN g=2 THEN coalesce(bump(1001),0) ELSE v+1000 END FROM generate_series(1,3) AS s(g) WHERE t.id=g RETURNING id,v",
        "MERGE INTO t USING generate_series(1,3) AS s(g) ON t.id=g WHEN MATCHED THEN UPDATE SET v=CASE WHEN g=2 THEN coalesce(bump(1001),0) ELSE v+1000 END RETURNING id,v"
    )]
    command: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("update.db"), backend);
    seed(&engine, true);
    engine
        .sql("INSERT INTO t VALUES (1,1),(2,2),(3,3)", &[])
        .unwrap();
    assert_eq!(pairs(&engine, command), [(1, 1001), (2, 1), (3, 1003)]);
    assert_eq!(
        pairs(&engine, "SELECT id,v FROM t"),
        [(1, 2001), (2, 1), (3, 1003)]
    );
}

#[rstest]
fn failed_insert_undoes_nested_publication(
    #[values("memory", "native", "kv", "redb")] backend: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("failure.db"), backend);
    seed(&engine, true);
    engine.sql("BEGIN; SAVEPOINT before_insert", &[]).unwrap();
    let error = engine
        .sql(
            "INSERT INTO t VALUES (1,1),(2,coalesce(bump(1),0)),(2,3)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"));
    engine.sql("ROLLBACK TO before_insert", &[]).unwrap();
    assert_eq!(pairs(&engine, "SELECT id,v FROM t").len(), 0);
    engine
        .sql(
            "INSERT INTO t VALUES (1,1),(2,coalesce(bump(1),0)); COMMIT",
            &[],
        )
        .unwrap();
    assert_eq!(pairs(&engine, "SELECT id,v FROM t"), [(1, 1001), (2, 1)]);
}

#[rstest]
fn delete_returning_preserves_nested_reinsertion(
    #[values("memory", "native", "kv", "redb")] backend: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("delete.db"), backend);
    seed(&engine, true);
    engine.sql("INSERT INTO t VALUES (1,1),(2,2),(3,3); CREATE FUNCTION revive(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ INSERT INTO t VALUES(k,9000) RETURNING id $$", &[]).unwrap();
    assert_eq!(
        pairs(
            &engine,
            "DELETE FROM t WHERE id<=2 RETURNING id, CASE WHEN id=2 THEN revive(1) ELSE v END AS v"
        ),
        [(1, 1), (2, 1)]
    );
    assert_eq!(pairs(&engine, "SELECT id,v FROM t"), [(1, 9000), (3, 3)]);
}

#[rstest]
#[case::key_update(false, false)]
#[case::key_reinsertion(false, true)]
#[case::partition_update(true, false)]
#[case::partition_reinsertion(true, true)]
fn moved_parent_rows_preserve_nested_publication(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[case] partitioned: bool,
    #[case] revive: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("moved.db"), backend);
    if partitioned {
        engine.sql("CREATE TABLE t (id integer, v integer) PARTITION BY RANGE (id); CREATE TABLE t_lo PARTITION OF t FOR VALUES FROM (0) TO (10); CREATE TABLE t_hi PARTITION OF t FOR VALUES FROM (10) TO (100); CREATE FUNCTION bump(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ UPDATE t SET v=v+1000 WHERE v=k RETURNING id $$", &[]).unwrap();
    } else {
        seed(&engine, true);
    }
    engine
        .sql("INSERT INTO t VALUES (1,1),(2,2),(3,3)", &[])
        .unwrap();
    if revive {
        engine.sql("CREATE FUNCTION revive(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ INSERT INTO t VALUES(k,9000) RETURNING id $$", &[]).unwrap();
        assert_eq!(
            pairs(
                &engine,
                "UPDATE t SET id=id+10,v=CASE WHEN id=2 THEN revive(1) ELSE v END RETURNING id,v"
            ),
            [(11, 1), (12, 1), (13, 3)]
        );
        assert_eq!(
            pairs(&engine, "SELECT id,v FROM t"),
            [(1, 9000), (11, 1), (12, 1), (13, 3)]
        );
    } else {
        assert_eq!(
            pairs(&engine, "UPDATE t SET id=id+10,v=CASE WHEN id=2 THEN coalesce(bump(1),0) ELSE v END RETURNING id,v"),
            [(11, 1), (12, 11), (13, 3)]
        );
        assert_eq!(
            pairs(&engine, "SELECT id,v FROM t"),
            [(11, 1001), (12, 11), (13, 3)]
        );
    }
}
