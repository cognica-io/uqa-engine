//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The open that first runs this version checks the tables an earlier version wrote. A table holding a row at an identity its integer key does not name resolves its keys through the key's index from then on; the others keep mapping their keys, and the check runs once.

use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

const VERIFIED: &str = "uqa.integer_key_identities.verified.v1";
const LIMIT: i64 = 1 << 62;

/// A native database with a table `clean` whose keys name their identities and a table `legacy` holding a negative key at identity 3, as an earlier version stored one, which then reads as a database that version wrote.
fn legacy_database(path: &std::path::Path) {
    {
        let engine = Engine::open(path).unwrap();
        engine
            .sql(
                "CREATE TABLE clean (id integer PRIMARY KEY, v integer);
                 INSERT INTO clean VALUES (1, 1), (2, 2);
                 CREATE TABLE legacy (id integer PRIMARY KEY, v integer);
                 INSERT INTO legacy VALUES (1, 1), (2, 2)",
                &[],
            )
            .unwrap();
        engine
            .add_document(
                "legacy",
                3,
                BTreeMap::from([("id".into(), Value::Int(-1)), ("v".into(), Value::Int(3))]),
            )
            .unwrap();
    }
    native_storage::catalog(ManagedConnection::open(path).unwrap())
        .unwrap()
        .delete_metadata(VERIFIED)
        .unwrap();
}

fn maps_keys(engine: &Engine, table: &str) -> bool {
    engine
        .require_table(table)
        .unwrap()
        .maps_integer_keys
        .load(Ordering::Acquire)
}

fn values(engine: &Engine, sql: &str, column: &str) -> Vec<Value> {
    engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| row[column].clone())
        .collect()
}

#[test]
fn the_first_open_resolves_the_keys_of_a_table_an_earlier_version_left_unmapped() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-integer-keys.db");
    legacy_database(&path);
    let engine = Engine::open(&path).unwrap();
    assert!(!maps_keys(&engine, "legacy"));
    assert!(maps_keys(&engine, "clean"));
    // Key 3 names the identity the negative key holds, yet no row has key 3.
    engine.sql("INSERT INTO legacy VALUES (3, 4)", &[]).unwrap();
    assert_eq!(
        values(&engine, "SELECT id FROM legacy ORDER BY id", "id"),
        [-1, 1, 2, 3].map(Value::Int)
    );
    assert_eq!(
        values(&engine, "SELECT v FROM legacy WHERE id = 3", "v"),
        [Value::Int(4)]
    );
    assert_eq!(
        values(&engine, "SELECT v FROM legacy WHERE id = -1", "v"),
        [Value::Int(3)]
    );
    assert_eq!(
        engine
            .sql("INSERT INTO legacy VALUES (3, 9)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    // The table no longer maps keys, so the new row takes a generated identity above every key-named one.
    let identities = engine
        .sql("SELECT v, _doc_id FROM legacy", &[])
        .unwrap()
        .rows;
    let identity = identities
        .iter()
        .find(|row| row["v"] == Value::Int(4))
        .map(|row| row["_doc_id"].clone());
    assert!(
        matches!(identity, Some(Value::Int(identity)) if identity >= LIMIT),
        "{identity:?}"
    );
    drop(engine);

    let reopened = Engine::open(&path).unwrap();
    assert!(!maps_keys(&reopened, "legacy"));
    assert!(maps_keys(&reopened, "clean"));
    assert_eq!(
        values(&reopened, "SELECT id FROM legacy ORDER BY id", "id"),
        [-1, 1, 2, 3].map(Value::Int)
    );
    let catalog = native_storage::catalog(ManagedConnection::open(&path).unwrap()).unwrap();
    assert_eq!(
        catalog.get_metadata(VERIFIED).unwrap().as_deref(),
        Some("true")
    );
}

#[test]
fn a_database_this_version_wrote_maps_the_keys_of_every_table() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checked-integer-keys.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE keyed (id integer PRIMARY KEY, v integer);
                 INSERT INTO keyed VALUES (1, 1), (-1, 2)",
                &[],
            )
            .unwrap();
        assert!(maps_keys(&engine, "keyed"));
    }
    let reopened = Engine::open(&path).unwrap();
    assert!(maps_keys(&reopened, "keyed"));
    assert_eq!(
        values(&reopened, "SELECT id FROM keyed ORDER BY id", "id"),
        [-1, 1].map(Value::Int)
    );
}
