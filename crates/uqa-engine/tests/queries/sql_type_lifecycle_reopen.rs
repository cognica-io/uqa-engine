//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type renames, schema moves, ownership and `USAGE` grants survive reopening every persistent provider: stored columns, views and routine signatures follow the type by identity and output spells its current name.

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

fn values(engine: &Engine, sql: &str) -> Vec<Value> {
    let result = engine.sql(sql, &[]).unwrap();
    (0..result.rows.len())
        .map(|row| result.value_at(row, 0).cloned().unwrap())
        .collect()
}

fn text(engine: &Engine, sql: &str) -> Vec<String> {
    values(engine, sql)
        .into_iter()
        .map(|value| match value {
            Value::Str(text) => text,
            other => panic!("{sql} returned {other:?}"),
        })
        .collect()
}

fn define(engine: &Engine) {
    for sql in [
        "CREATE ROLE lifecycle_reader",
        "CREATE SCHEMA lifecycle_moved",
        "CREATE TYPE lifecycle_mood AS ENUM ('sad', 'ok')",
        "CREATE DOMAIN lifecycle_positive AS integer CHECK (VALUE > 0)",
        "CREATE TABLE lifecycle_rows (m lifecycle_mood, p lifecycle_positive)",
        "INSERT INTO lifecycle_rows VALUES ('ok', 1)",
        "CREATE VIEW lifecycle_view AS SELECT m, m = 'ok'::lifecycle_mood AS is_ok FROM lifecycle_rows",
        "CREATE FUNCTION lifecycle_check(x lifecycle_mood) RETURNS boolean LANGUAGE sql RETURN x = 'ok'::lifecycle_mood",
        "REVOKE USAGE ON TYPE lifecycle_mood FROM PUBLIC",
        "GRANT USAGE ON TYPE lifecycle_mood TO lifecycle_reader",
        "ALTER TYPE lifecycle_mood RENAME TO lifecycle_feeling",
        "ALTER TYPE lifecycle_feeling SET SCHEMA lifecycle_moved",
        "ALTER DOMAIN lifecycle_positive OWNER TO lifecycle_reader",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
}

fn verify(engine: &Engine, type_name: &str) {
    assert_eq!(
        text(
            engine,
            "SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'lifecycle_rows'::regclass AND attnum > 0 ORDER BY attnum",
        ),
        [format!("lifecycle_moved.{type_name}"), "lifecycle_positive".into()]
    );
    assert_eq!(text(engine, "SELECT m::text FROM lifecycle_rows"), ["ok"]);
    assert_eq!(
        text(engine, "SELECT pg_get_viewdef('lifecycle_view'::regclass)"),
        [format!(
            " SELECT m,\n    (m = 'ok'::lifecycle_moved.{type_name}) AS is_ok\n   FROM lifecycle_rows;"
        )]
    );
    assert_eq!(
        text(
            engine,
            "SELECT 'lifecycle_check'::regproc::regprocedure::text",
        ),
        [format!("lifecycle_check(lifecycle_moved.{type_name})")]
    );
    assert_eq!(
        values(
            engine,
            &format!(
                "SELECT has_type_privilege('lifecycle_reader', 'lifecycle_moved.{type_name}', 'USAGE') AND NOT has_type_privilege('public', 'lifecycle_moved.{type_name}', 'USAGE')"
            ),
        ),
        [Value::Bool(true)]
    );
    assert_eq!(
        text(
            engine,
            "SELECT pg_get_userbyid(typowner)::text FROM pg_type WHERE oid = 'lifecycle_positive'::regtype",
        ),
        ["lifecycle_reader"]
    );
    assert_eq!(
        values(
            engine,
            &format!("SELECT lifecycle_check('ok'::lifecycle_moved.{type_name})"),
        ),
        [Value::Bool(true)]
    );
}

fn reopen_lifecycle(open: fn(&Path) -> Engine, file: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(file);
    {
        let engine = open(&path);
        define(&engine);
        verify(&engine, "lifecycle_feeling");
    }
    {
        let engine = open(&path);
        verify(&engine, "lifecycle_feeling");
        engine
            .sql(
                "ALTER TYPE lifecycle_moved.lifecycle_feeling RENAME TO lifecycle_state",
                &[],
            )
            .unwrap();
    }
    verify(&open(&path), "lifecycle_state");
}

#[test]
fn type_lifecycle_survives_reopening_sqlite() {
    reopen_lifecycle(open_sqlite, "lifecycle.db");
}

#[test]
fn type_lifecycle_survives_reopening_sqlite_key_value() {
    reopen_lifecycle(open_sqlite_key_value, "lifecycle.db");
}

#[test]
fn type_lifecycle_survives_reopening_redb() {
    reopen_lifecycle(open_redb, "lifecycle.redb");
}
