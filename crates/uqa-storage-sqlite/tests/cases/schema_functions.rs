//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-owned maintenance connections retain native expression indexes without acquiring logical writer authority.

use std::{collections::BTreeMap, path::Path};

use rusqlite::{Connection, OpenFlags};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentStore, ValueIndexKey};
use uqa_storage_sqlite::{
    register_schema_functions, Catalog, ManagedConnection, SQLiteBTreeIndexStore,
    SQLiteDocumentStore,
};

const KEY: &str = "schema function fixture key";

fn managed(path: &Path, encrypted: bool) -> ManagedConnection {
    if encrypted {
        ManagedConnection::open_encrypted(path, KEY)
    } else {
        ManagedConnection::open(path)
    }
    .unwrap()
}

fn fixture(path: &Path, encrypted: bool) {
    let connection = managed(path, encrypted);
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    for id in [1, 2] {
        documents
            .put(id, BTreeMap::from([("n".into(), Value::Int(id as i64))]))
            .unwrap();
    }
    SQLiteBTreeIndexStore::new(connection.clone())
        .replace(
            "items",
            &ValueIndexKey::Column("n".into()),
            &[(1, Value::Int(1)), (2, Value::Int(2))],
        )
        .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn raw(path: &Path, encrypted: bool, flags: OpenFlags) -> Connection {
    let connection = Connection::open_with_flags(path, flags).unwrap();
    if encrypted {
        connection.pragma_update(None, "key", KEY).unwrap();
    }
    connection
}

fn integrity(connection: &Connection) {
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn read_only_integrity_checks_support_native_expression_indexes() {
    for encrypted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.db");
        fixture(&path, encrypted);
        let connection = raw(&path, encrypted, OpenFlags::SQLITE_OPEN_READ_ONLY);
        let error = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap_err();
        assert!(error.to_string().contains("__uqa_btree_equal_v1"));
        register_schema_functions(&connection).unwrap();
        register_schema_functions(&connection).unwrap();
        integrity(&connection);
        let error = connection
            .execute("CREATE TABLE forbidden (id INTEGER)", [])
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
    }
}

#[test]
fn registration_preserves_transaction_configuration_and_write_guards() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    fixture(&path, false);
    let connection = raw(&path, false, OpenFlags::SQLITE_OPEN_READ_WRITE);
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .unwrap();
    connection.execute_batch("BEGIN").unwrap();
    let state = |connection: &Connection| {
        connection
            .query_row(
                "SELECT (SELECT journal_mode FROM pragma_journal_mode), \
                        (SELECT synchronous FROM pragma_synchronous), \
                        (SELECT schema_version FROM pragma_schema_version), \
                        (SELECT count(*) FROM sqlite_schema)",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .unwrap()
    };
    let before = state(&connection);
    register_schema_functions(&connection).unwrap();
    register_schema_functions(&connection).unwrap();
    assert!(!connection.is_autocommit());
    assert_eq!(state(&connection), before);
    integrity(&connection);
    assert!(connection
        .prepare("SELECT __uqa_mvcc_write_permit()")
        .is_err());
    assert!(connection
        .execute("DELETE FROM _uqa_mvcc_transactions", [])
        .is_err());
    connection.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn sqlcipher_export_preserves_native_indexes_in_both_encryption_directions() {
    for source_encrypted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let target = directory.path().join("target.db");
        fixture(&source, source_encrypted);
        let connection = raw(
            &source,
            source_encrypted,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        );
        register_schema_functions(&connection).unwrap();
        connection
            .execute(
                "ATTACH DATABASE ?1 AS exported KEY ?2",
                rusqlite::params![
                    target.to_str().unwrap(),
                    if source_encrypted { "" } else { KEY }
                ],
            )
            .unwrap();
        connection
            .execute_batch("SELECT sqlcipher_export('exported'); DETACH DATABASE exported;")
            .unwrap();
        let exported = raw(&target, !source_encrypted, OpenFlags::SQLITE_OPEN_READ_ONLY);
        register_schema_functions(&exported).unwrap();
        integrity(&exported);
        drop(exported);

        let reopened = managed(&target, !source_encrypted);
        reopened
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let indexes = SQLiteBTreeIndexStore::new(reopened.clone());
        assert_eq!(
            indexes
                .probe_equal(
                    "items",
                    &ValueIndexKey::Column("n".into()),
                    &Value::Float(2.0)
                )
                .unwrap(),
            Some(vec![2])
        );
        assert_eq!(
            SQLiteDocumentStore::new(reopened, "items").get(2).unwrap(),
            Some(BTreeMap::from([("n".into(), Value::Int(2))]))
        );
    }
}
