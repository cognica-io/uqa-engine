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
        transaction.execute_batch("CREATE INDEX _btree_index_value_idx ON _btree_index_entries(table_name, field, value_json, doc_id); DROP TABLE _uqa_mvcc_native_format")?;
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

#[test]
fn ordered_key_upgrade_writes_every_key_again_and_rolls_back_with_its_format_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    let entries: Vec<_> = (1..=40_u64)
        .map(|id| (id, Value::Int(i64::try_from(id).unwrap() * 10 - 200)))
        .collect();
    for (id, value) in &entries {
        documents
            .put(*id, [("key".into(), value.clone())].into())
            .unwrap();
    }
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    indexes.replace("items", &"key".into(), &entries).unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let namespace = records.native_namespace().unwrap();
    // The format before this one: the same projection, with keys that are equal for equal values and in no order.
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        for (name, _) in crate::btree_index::equality::triggers() {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        transaction.execute_batch("DELETE FROM _btree_equality_keys; INSERT INTO _btree_equality_keys SELECT table_name, field, doc_id, __uqa_btree_equal_v1(value_json) FROM _btree_index_entries; DROP TABLE _uqa_mvcc_native_format;")?;
        for (_, sql) in crate::btree_index::equality::equal_key_triggers() {
            transaction.execute_batch(&sql)?;
        }
        transaction.execute_batch(FORMAT_SEVENTEEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,17,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 17)?;
        assert!(validate_format(sqlite, CURRENT_VERSION).is_err());
        Ok(())
    });
    let unordered = |sqlite: &Connection| -> rusqlite::Result<i64> {
        // A key that orders as its value does puts a negative number before a positive one.
        sqlite.query_row(
            "SELECT count(*) FROM _btree_equality_keys low JOIN _btree_equality_keys high ON high.doc_id = low.doc_id + 1 WHERE low.equality_key >= high.equality_key",
            [],
            |row| row.get(0),
        )
    };
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        assert_ne!(unordered(sqlite)?, 0);
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        assert_eq!(
            initialize_in(&transaction, &control)?.namespace.0,
            namespace
        );
        validate_format(&transaction, CURRENT_VERSION)?;
        assert_eq!(unordered(&transaction)?, 0);
        assert!(check_mapping_version(&transaction, 17).is_err());
        drop(transaction);
        validate_format(sqlite, 17)?;
        assert_ne!(unordered(sqlite)?, 0);
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
        indexes
            .probe_equal("items", &"key".into(), &Value::Float(-100.0))
            .unwrap(),
        Some(vec![10])
    );
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert_eq!(unordered(sqlite)?, 0);
        let earlier: i64 = sqlite.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE sql LIKE '%__uqa_btree_equal_v1%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(earlier, 0);
        Ok(())
    });
    // A write after the upgrade keeps the order.
    let lowest = Value::Float(-1000.5);
    documents
        .put(41, [("key".into(), lowest.clone())].into())
        .unwrap();
    let mut entries = entries;
    entries.push((41, lowest));
    indexes.replace("items", &"key".into(), &entries).unwrap();
    with(&connection, |sqlite| {
        assert_eq!(
            sqlite.query_row(
                "SELECT doc_id FROM _btree_equality_keys ORDER BY equality_key LIMIT 1",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            41
        );
        Ok(())
    });
}
