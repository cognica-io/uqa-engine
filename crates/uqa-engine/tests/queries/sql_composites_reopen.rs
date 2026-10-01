//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone composite types survive reopening every persistent provider: the type, its array type and its relation keep their OIDs, stored values keep their fields through renames, cascaded attribute removal and field assignment, and a rolled-back definition leaves nothing behind.

use std::path::Path;
use std::sync::Arc;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::SQLiteKeyValueStorage;

fn open_sqlite(path: &Path) -> Engine {
    Engine::open(path).unwrap()
}

fn open_sqlite_key_value(path: &Path) -> Engine {
    let storage: Arc<dyn PersistentStorageProvider> =
        Arc::new(SQLiteKeyValueStorage::open(path).unwrap());
    Engine::from_persistent_provider(storage).unwrap()
}

fn open_redb(path: &Path) -> Engine {
    let storage: Arc<dyn PersistentStorageProvider> = Arc::new(RedbStorage::open(path).unwrap());
    Engine::from_persistent_provider(storage).unwrap()
}

fn run(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn text(engine: &Engine, sql: &str) -> Vec<String> {
    let result = engine.sql(sql, &[]).unwrap();
    (0..result.rows.len())
        .map(|row| match result.value_at(row, 0) {
            Some(Value::Str(text)) => text.clone(),
            other => panic!("{sql} returned {other:?}"),
        })
        .collect()
}

fn oids(engine: &Engine) -> Vec<String> {
    text(
        engine,
        "SELECT concat_ws(',', c.oid, t.oid, t.typarray) FROM pg_class c JOIN pg_type t ON t.typrelid = c.oid WHERE c.relname = 'reopen_pair'",
    )
}

fn define(engine: &Engine) {
    for sql in [
        "CREATE TYPE reopen_mood AS ENUM ('sad', 'happy')",
        "CREATE TYPE reopen_pair AS (x integer, y text, m reopen_mood)",
        "CREATE TYPE reopen_outer AS (p reopen_pair, tags reopen_pair[], n integer)",
        "CREATE TABLE reopen_rows (id integer, p reopen_pair, o reopen_outer)",
        "INSERT INTO reopen_rows VALUES (1, ROW(1, 'one', 'happy'), ROW(ROW(2, 'two', 'sad'), ARRAY[ROW(3, 'three', NULL)::reopen_pair], 4))",
        "INSERT INTO reopen_rows VALUES (2, '(5,\"five five\",sad)', NULL)",
        "UPDATE reopen_rows SET p.y = 'ONE', o.n = 40 WHERE id = 1",
        "BEGIN",
        "CREATE TYPE reopen_rolled AS (a integer)",
        "ROLLBACK",
    ] {
        run(engine, sql);
    }
}

fn verify(engine: &Engine, pair: &str, rows: &[&str]) {
    assert_eq!(
        text(
            engine,
            "SELECT concat_ws('|', id, p::text, o::text) FROM reopen_rows ORDER BY id"
        ),
        rows
    );
    assert_eq!(
        text(engine, &format!("SELECT '(7,\"a b\",happy)'::{pair}::text")),
        [String::from("(7,\"a b\",happy)")]
    );
    assert_eq!(
        text(
            engine,
            "SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'reopen_outer'::regclass ORDER BY attnum"
        ),
        [pair.to_string(), format!("{pair}[]"), "integer".into()]
    );
    assert_eq!(
        text(
            engine,
            "SELECT CASE WHEN to_regtype('reopen_rolled') IS NULL THEN 'absent' ELSE 'present' END"
        ),
        ["absent"]
    );
}

fn reopen_composites(open: fn(&Path) -> Engine, file: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(file);
    let before = [
        "1|(1,ONE,happy)|(\"(2,two,sad)\",\"{\"\"(3,three,)\"\"}\",40)",
        "2|(5,\"five five\",sad)",
    ];
    let identities;
    {
        let engine = open(&path);
        define(&engine);
        verify(&engine, "reopen_pair", &before);
        identities = oids(&engine);
        assert_eq!(identities.len(), 1);
    }
    {
        let engine = open(&path);
        verify(&engine, "reopen_pair", &before);
        assert_eq!(oids(&engine), identities);
        run(&engine, "DROP TYPE reopen_mood CASCADE");
        run(&engine, "ALTER TYPE reopen_pair RENAME TO reopen_couple");
    }
    let engine = open(&path);
    assert_eq!(
        text(
            &engine,
            "SELECT concat_ws('|', id, p::text, o::text) FROM reopen_rows ORDER BY id"
        ),
        [
            "1|(1,ONE)|(\"(2,two)\",\"{\"\"(3,three)\"\"}\",40)",
            "2|(5,\"five five\")"
        ]
    );
    assert_eq!(
        text(
            &engine,
            "SELECT attname::text FROM pg_attribute WHERE attrelid = 'reopen_couple'::regclass ORDER BY attnum"
        ),
        ["x", "y", "........pg.dropped.3........"]
    );
    assert_eq!(
        text(
            &engine,
            "SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'reopen_outer'::regclass ORDER BY attnum"
        ),
        ["reopen_couple", "reopen_couple[]", "integer"]
    );
}

#[test]
fn composite_types_survive_reopening_sqlite() {
    reopen_composites(open_sqlite, "composites.db");
}

#[test]
fn composite_types_survive_reopening_sqlite_key_value() {
    reopen_composites(open_sqlite_key_value, "composites.db");
}

#[test]
fn composite_types_survive_reopening_redb() {
    reopen_composites(open_redb, "composites.redb");
}
