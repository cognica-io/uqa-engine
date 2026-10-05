//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation row type catalogs and namespace lifecycle against an independent `PostgreSQL` reference.

use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../../tests/parity/pg18/relation_row_type_oracle.expected.json");

fn open(provider: usize, path: &Path) -> Engine {
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

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn relation_row_types_match_postgresql(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("relation-row-types.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, ORACLE);
    if provider != 0 {
        drop(engine);
        let reopened = open(provider, &path);
        let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
        for expected in oracle["cases"].as_array().unwrap().iter().filter(|case| {
            case["id"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("reopen."))
        }) {
            let sql = expected["sql"].as_str().unwrap();
            let actual = crate::pg18_oracle::run_case(&reopened, sql);
            assert_eq!(actual["error"], expected["error"], "reopened: {sql}");
            assert_eq!(actual["results"], expected["results"], "reopened: {sql}");
        }
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn peer_catalog_refresh_preserves_displaced_array_identity(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("peer-row-types.db");
    let first = open(provider, &path);
    first
        .sql("CREATE TABLE peer_item(id integer)", &[])
        .unwrap();
    let second = first.new_session().unwrap();
    let before = second
        .sql("SELECT 'peer_item[]'::regtype::oid AS identity", &[])
        .unwrap();
    first
        .sql("CREATE TYPE _peer_item AS ENUM ('v')", &[])
        .unwrap();
    let displaced = second
        .sql("SELECT 'peer_item[]'::regtype::oid AS identity", &[])
        .unwrap();
    assert_eq!(before.rows, displaced.rows);
    let array_name = second.sql("SELECT a.typname::text AS name FROM pg_type t JOIN pg_type a ON a.oid=t.typarray WHERE t.oid='peer_item'::regtype", &[]).unwrap();
    assert_eq!(
        array_name.rows[0]["name"],
        uqa_core::Value::Str("__peer_item".into())
    );
    first
        .sql("ALTER TABLE peer_item RENAME TO peer_renamed", &[])
        .unwrap();
    let renamed = second
        .sql("SELECT 'peer_renamed[]'::regtype::oid AS identity", &[])
        .unwrap();
    assert_eq!(before.rows, renamed.rows);
    second.sql("BEGIN; SAVEPOINT before_displace; CREATE TYPE _peer_renamed AS ENUM ('x'); ROLLBACK TO before_displace; COMMIT", &[]).unwrap();
    let restored = first.sql("SELECT a.typname::text AS name FROM pg_type t JOIN pg_type a ON a.oid=t.typarray WHERE t.oid='peer_renamed'::regtype", &[]).unwrap();
    assert_eq!(
        restored.rows[0]["name"],
        uqa_core::Value::Str("_peer_renamed".into())
    );
}
