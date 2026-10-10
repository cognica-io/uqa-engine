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
