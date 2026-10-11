//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::tests::{materialization::with, namespace_upgrade::preserved};
use crate::{SQLiteBTreeIndexStore, SQLiteDocumentStore};
use uqa_core::Value;
use uqa_storage::DocumentStore;

#[test]
fn equality_index_upgrade_preserves_history_and_rolls_back_with_its_format_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let key = Value::Row(vec![Value::Str("group".into()), Value::Int(7)].into());
    SQLiteDocumentStore::new(connection.clone(), "items")
        .put(1, [("key".into(), key.clone())].into())
        .unwrap();
    let index = SQLiteBTreeIndexStore::new(connection.clone());
    index
        .replace("items", &"key".into(), &[(1, key.clone())])
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let namespace = records.native_namespace().unwrap();
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        for (name, _) in crate::btree_index::equality::triggers() {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        transaction.execute_batch("DROP TABLE _btree_equality_keys")?;
        for (name, _) in crate::btree_index::coverage::triggers() {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        for (name, _) in crate::btree_index::coverage::TABLES {
            transaction.execute_batch(&format!("DROP TABLE {name}"))?;
        }
        transaction.execute_batch("DROP INDEX _btree_index_equal_v1; CREATE INDEX _btree_index_value_idx ON _btree_index_entries(table_name, field, value_json, doc_id); DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_FIFTEEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,15,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 15)?;
        Ok(())
    });
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let upgraded = initialize_in(&transaction, &control)?;
        assert_eq!(upgraded.namespace.0, namespace);
        check_mapping_version(&transaction, CURRENT_VERSION)?;
        assert!(check_mapping_version(&transaction, 15).is_err());
        drop(transaction);
        validate_format(sqlite, 15)?;
        assert!(!sqlite.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = '_btree_index_equal_v1')",
            [],
            |row| row.get::<_, bool>(0)
        )?);
        for (name, _) in crate::btree_index::coverage::TABLES {
            assert!(!sqlite.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
                [name],
                |row| row.get::<_, bool>(0)
            )?);
        }
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.native_namespace(), Some(namespace));
    assert_eq!(preserved(&connection), before);
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    assert_eq!(
        index.probe_equal("items", &"key".into(), &key).unwrap(),
        Some(vec![1])
    );
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert!(!sqlite.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = '_btree_index_value_idx')",
            [],
            |row| row.get::<_, bool>(0)
        )?);
        Ok(())
    });
}

#[test]
fn expression_index_upgrade_preserves_history_and_rolls_back_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let key = Value::Row(vec![Value::Str("group".into()), Value::Int(7)].into());
    SQLiteDocumentStore::new(connection.clone(), "items")
        .put(1, [("key".into(), key.clone())].into())
        .unwrap();
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    indexes
        .replace("items", &"key".into(), &[(1, key)])
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let namespace = records.native_namespace().unwrap();
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        for (name, _) in crate::btree_index::equality::triggers() {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        transaction.execute_batch("DROP INDEX _btree_index_equal_v1; DROP TABLE _btree_equality_keys; DROP TABLE _uqa_mvcc_native_format;")?;
        transaction.execute_batch(crate::btree_index::equality::LEGACY_INDEX_SQL)?;
        transaction.execute_batch(FORMAT_SIXTEEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,16,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 16)?;
        Ok(())
    });
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        assert_eq!(
            initialize_in(&transaction, &control)?.namespace.0,
            namespace
        );
        validate_format(&transaction, CURRENT_VERSION)?;
        assert!(check_mapping_version(&transaction, 16).is_err());
        drop(transaction);
        validate_format(sqlite, 16)?;
        assert!(!sqlite.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = '_btree_equality_keys')",
            [],
            |row| row.get::<_, bool>(0),
        )?);
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.native_namespace(), Some(namespace));
    assert_eq!(preserved(&connection), before);
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let target = Value::Row(vec![Value::Str("group".into()), Value::Float(7.0)].into());
    assert_eq!(
        indexes
            .probe_equal("items", &"key".into(), &target)
            .unwrap(),
        Some(vec![1])
    );
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        let expression_indexes: i64 = sqlite.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'index' AND sql LIKE '%__uqa_btree_equal_v1%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(expression_indexes, 0);
        Ok(())
    });
}
