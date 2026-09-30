//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every catalog draws the OIDs of new objects from one database counter: in creation order across commands, in `PostgreSQL`'s order within a command, and unchanged by reopening.

use std::path::Path;
use std::sync::Arc;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::SQLiteKeyValueStorage;

fn oid(engine: &Engine, sql: &str) -> i64 {
    let result = engine.sql(sql, &[]).unwrap();
    assert_eq!(result.rows.len(), 1, "{sql}");
    match result.value_at(0, 0) {
        Some(Value::Int(oid)) => *oid,
        other => panic!("{sql} returned {other:?}"),
    }
}

fn oids(engine: &Engine, sql: &str) -> Vec<i64> {
    let result = engine.sql(sql, &[]).unwrap();
    (0..result.rows.len())
        .map(|row| match result.value_at(row, 0) {
            Some(Value::Int(oid)) => *oid,
            other => panic!("{sql} returned {other:?}"),
        })
        .collect()
}

fn define(engine: &Engine) {
    for sql in [
        "CREATE SCHEMA ordered",
        "CREATE TYPE ordered.mood AS ENUM ('sad', 'ok')",
        "CREATE DOMAIN ordered.positive AS integer CONSTRAINT positive_check CHECK (VALUE > 0)",
        "CREATE TABLE ordered.items (id integer CONSTRAINT items_pkey PRIMARY KEY, mood ordered.mood)",
        "CREATE VIEW ordered.item_view AS SELECT id FROM ordered.items",
        "CREATE FUNCTION ordered.touch() RETURNS integer LANGUAGE sql RETURN 1",
        "CREATE ROLE ordered_role",
        "ALTER TYPE ordered.mood ADD VALUE 'happy'",
        "ALTER TYPE ordered.mood ADD VALUE 'meh' BEFORE 'ok'",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
}

/// The OIDs of every defined object, in the order `PostgreSQL` allocates them.
fn creation_order(engine: &Engine) -> Vec<(&'static str, i64)> {
    let labels = oids(
        engine,
        "SELECT oid::bigint FROM pg_enum WHERE enumtypid = 'ordered.mood'::regtype ORDER BY oid",
    );
    vec![
        (
            "schema",
            oid(engine, "SELECT 'ordered'::regnamespace::oid::bigint"),
        ),
        (
            "enum array",
            oid(
                engine,
                "SELECT typarray::bigint FROM pg_type WHERE oid = 'ordered.mood'::regtype",
            ),
        ),
        (
            "enum",
            oid(engine, "SELECT 'ordered.mood'::regtype::oid::bigint"),
        ),
        ("first label", labels[0]),
        ("second label", labels[1]),
        (
            "domain array",
            oid(
                engine,
                "SELECT typarray::bigint FROM pg_type WHERE oid = 'ordered.positive'::regtype",
            ),
        ),
        (
            "domain",
            oid(engine, "SELECT 'ordered.positive'::regtype::oid::bigint"),
        ),
        (
            "domain constraint",
            oid(
                engine,
                "SELECT oid::bigint FROM pg_constraint WHERE conname = 'positive_check'",
            ),
        ),
        (
            "table",
            oid(engine, "SELECT 'ordered.items'::regclass::oid::bigint"),
        ),
        (
            "table row type",
            oid(
                engine,
                "SELECT reltype::bigint FROM pg_class WHERE oid = 'ordered.items'::regclass",
            ),
        ),
        (
            "key index",
            oid(engine, "SELECT 'ordered.items_pkey'::regclass::oid::bigint"),
        ),
        (
            "key constraint",
            oid(
                engine,
                "SELECT oid::bigint FROM pg_constraint WHERE conname = 'items_pkey'",
            ),
        ),
        (
            "view",
            oid(engine, "SELECT 'ordered.item_view'::regclass::oid::bigint"),
        ),
        (
            "view row type",
            oid(
                engine,
                "SELECT reltype::bigint FROM pg_class WHERE oid = 'ordered.item_view'::regclass",
            ),
        ),
        (
            "view rule",
            oid(
                engine,
                "SELECT oid::bigint FROM pg_rewrite WHERE ev_class = 'ordered.item_view'::regclass",
            ),
        ),
        (
            "function",
            oid(engine, "SELECT 'ordered.touch'::regproc::oid::bigint"),
        ),
        (
            "role",
            oid(engine, "SELECT 'ordered_role'::regrole::oid::bigint"),
        ),
        ("added label", labels[2]),
        ("inserted label", labels[3]),
    ]
}

fn assert_creation_order(order: &[(&str, i64)]) {
    for pair in order.windows(2) {
        assert!(pair[0].1 < pair[1].1, "{order:?}");
    }
}

#[test]
fn objects_take_oids_in_creation_order_from_first_normal_object_id() {
    let engine = Engine::new();
    define(&engine);
    let order = creation_order(&engine);
    assert_eq!(order[0].1, 16_384, "{order:?}");
    assert_creation_order(&order);
    // `EnumValuesCreate` gives new labels even OIDs; `AddEnumLabel` keeps an even OID only for a label that orders correctly by OID, so a label inserted before an older one takes an odd OID.
    let labels = order[3..=4]
        .iter()
        .chain(&order[order.len() - 2..order.len() - 1])
        .map(|(_, oid)| oid % 2)
        .collect::<Vec<_>>();
    assert_eq!(labels, [0, 0, 0], "{order:?}");
    assert_eq!(order[order.len() - 1].1 % 2, 1, "{order:?}");
}

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

fn oids_survive_reopening(open: fn(&Path) -> Engine, file: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(file);
    let before = {
        let engine = open(&path);
        define(&engine);
        // An aborted command still consumes its OIDs, as the counter advances outside transactions.
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql("CREATE TABLE ordered.discarded (id integer)", &[])
            .unwrap();
        engine.sql("ROLLBACK", &[]).unwrap();
        creation_order(&engine)
    };
    let engine = open(&path);
    assert_eq!(creation_order(&engine), before);
    assert_creation_order(&before);
    engine
        .sql("CREATE TABLE ordered.later (id integer)", &[])
        .unwrap();
    let later = oid(&engine, "SELECT 'ordered.later'::regclass::oid::bigint");
    // The table rolled back before reopening took the OIDs after the last defined object.
    assert!(later > before[before.len() - 1].1 + 3, "{before:?} {later}");
}

#[test]
fn oids_survive_reopening_sqlite() {
    oids_survive_reopening(open_sqlite, "oids.db");
}

#[test]
fn oids_survive_reopening_sqlite_key_value() {
    oids_survive_reopening(open_sqlite_key_value, "oids.db");
}

#[test]
fn oids_survive_reopening_redb() {
    oids_survive_reopening(open_redb, "oids.redb");
}
