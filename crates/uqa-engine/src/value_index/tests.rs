//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_storage::document_store::{Document, DocumentStore, StoredDocument};

#[derive(Clone)]
struct MissingProjectionStore;

impl DocumentStore for MissingProjectionStore {
    fn put_stored(
        &mut self,
        _doc_id: DocId,
        _document: StoredDocument,
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok((doc_id == 1).then(|| StoredDocument::new(Document::new())))
    }

    fn put(&mut self, _doc_id: DocId, _document: Document) -> StorageBackendResult<()> {
        Ok(())
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        Ok((doc_id == 1).then(Document::new))
    }

    fn delete(&mut self, _doc_id: DocId) -> StorageBackendResult<()> {
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn get_fields_multi(
        &self,
        _doc_ids: &[DocId],
        _fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        Ok(BTreeMap::new())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        Ok(vec![1])
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(1)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        Ok(Box::new(self.clone()))
    }
}

#[test]
fn rebuild_rejects_a_document_missing_from_the_field_projection() {
    let engine = crate::Engine::new();
    engine
        .sql("CREATE TABLE projection_gap (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let table = engine.try_table("projection_gap").unwrap().unwrap();
    *table.document_store.write() = Box::new(MissingProjectionStore);
    crate::Engine::value_indexes_clear(&table);

    let error = engine
        .ensure_value_index("projection_gap", &"id".into())
        .unwrap_err();
    assert!(error.to_string().contains("lost document 1"), "{error}");
    assert!(table.value_indexes.read().is_empty());
}

#[test]
fn relation_key_suffix_preserves_quoted_components() {
    assert_eq!(unqualified_relation_key("public.items"), Some("items"));
    assert_eq!(
        unqualified_relation_key("public.\"items.with.dot\""),
        Some("\"items.with.dot\"")
    );
    assert_eq!(
        unqualified_relation_key("\"schema.with.dot\".\"items.with.dot\""),
        Some("\"items.with.dot\"")
    );
    assert_eq!(
        unqualified_relation_key("public.\"items\"\"quoted\""),
        Some("\"items\"\"quoted\"")
    );
}

#[test]
fn query_builds_missing_durable_index_in_memory_only() {
    let directory = tempfile::tempdir().unwrap();
    let engine = crate::Engine::open(&directory.path().join("memory-only-btree.db")).unwrap();
    engine
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO items (id) VALUES (1)", &[])
        .unwrap();
    let backend = engine.storage.backend.as_ref().unwrap();
    backend
        .drop_btree_index("public.items", &"id".into())
        .unwrap();
    let table = engine.try_table("items").unwrap().unwrap();
    crate::Engine::value_indexes_clear(&table);

    let result = engine
        .sql("SELECT id FROM items WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert!(backend
        .load_btree_index("public.items", &"id".into())
        .unwrap()
        .is_none());
    assert!(engine
        .try_table("items")
        .unwrap()
        .unwrap()
        .value_indexes
        .read()
        .contains_key(&"id".into()));

    // Recovery leaves a missing durable index cold, so it cannot execute SQL callbacks while the transaction state is locked. Its next read builds only the in-memory accelerator.
    engine.drop_persistent_value_indexes();
    assert!(backend
        .load_btree_index("public.items", &"id".into())
        .unwrap()
        .is_none());
    assert!(table.value_indexes.read().is_empty());
    assert_eq!(
        engine
            .sql("SELECT id FROM items WHERE id=1", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(table.value_indexes.read().contains_key(&"id".into()));
    assert!(backend
        .load_btree_index("public.items", &"id".into())
        .unwrap()
        .is_none());

    // The explicit persistence path must not mistake the hot memory cache
    // for a durable marker.
    engine
        .ensure_persistent_value_index("items", &"id".into())
        .unwrap();
    assert_eq!(
        backend
            .load_btree_index("public.items", &"id".into())
            .unwrap()
            .unwrap(),
        vec![(1, Value::Int(1))]
    );
}

#[test]
fn open_repair_discards_raw_alias_and_rebuilds_canonical_index() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("repair-btree.db");
    let engine = crate::tests::native_storage::legacy_engine(&database);
    engine
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO items (id) VALUES (1)", &[])
        .unwrap();
    let backend = engine.storage.backend.as_ref().unwrap().clone();
    backend
        .drop_btree_index("public.items", &"id".into())
        .unwrap();
    backend
        .replace_btree_index("public.items", &"obsolete".into(), &[(1, Value::Int(888))])
        .unwrap();
    // Bypass the v21 guard only to inject a pre-v17 unqualified alias that
    // a current engine would never create, then restore the guard before
    // exercising open repair.
    let raw = rusqlite::Connection::open(&database).unwrap();
    raw.execute("DROP TRIGGER _btree_entries_document_insert", [])
        .unwrap();
    backend
        .replace_btree_index("items", &"id".into(), &[(1, Value::Int(999))])
        .unwrap();
    raw.execute_batch(
        "CREATE TRIGGER _btree_entries_document_insert
                 BEFORE INSERT ON _btree_index_entries
                 WHEN NOT EXISTS (
                     SELECT 1 FROM _documents
                      WHERE table_name = NEW.table_name AND doc_id = NEW.doc_id
                 )
                 BEGIN
                     SELECT RAISE(ABORT, 'persistent B-tree entry has no backing document');
                 END;",
    )
    .unwrap();
    drop(raw);
    let table = engine.try_table("items").unwrap().unwrap();
    crate::Engine::value_indexes_clear(&table);
    drop(table);
    drop(backend);
    drop(engine);

    let reopened = crate::Engine::open(&database).unwrap();
    let backend = reopened.storage.backend.as_ref().unwrap();

    assert!(backend
        .load_btree_index("items", &"id".into())
        .unwrap()
        .is_none());
    assert!(backend
        .load_btree_index("public.items", &"obsolete".into())
        .unwrap()
        .is_none());
    assert_eq!(
        backend
            .load_btree_index("public.items", &"id".into())
            .unwrap()
            .unwrap(),
        vec![(1, Value::Int(1))]
    );
}

#[test]
fn clean_open_repair_does_not_contend_for_sqlite_writer_lock() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("clean-repair.db");
    let engine = crate::Engine::open(&database).unwrap();
    engine
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO items (id) VALUES (1)", &[])
        .unwrap();
    assert!(engine
        .persistent_value_index_repair_plan()
        .unwrap()
        .is_empty());

    // A clean repair is read-only and therefore succeeds while an
    // independent session owns SQLite's single writer reservation. If the
    // repair unconditionally issued BEGIN IMMEDIATE this would block and
    // eventually return SQLITE_BUSY.
    let blocker = engine
        .storage
        .provider
        .as_ref()
        .unwrap()
        .open_session()
        .unwrap();
    blocker.backend.begin_transaction().unwrap();
    let repair_result = engine.repair_persistent_value_indexes_on_open();
    let new_session_result = engine.new_session();
    let reopen_result = crate::Engine::open(&database);
    blocker.backend.rollback_transaction().unwrap();
    repair_result.unwrap();
    new_session_result.unwrap();
    reopen_result.unwrap();
}

#[test]
fn memory_value_indexes_survive_their_own_data_generations() {
    let engine = crate::Engine::new();
    engine
        .sql("CREATE TABLE kept (id INTEGER PRIMARY KEY, v INTEGER)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO kept SELECT g, g FROM generate_series(1, 50) AS g",
            &[],
        )
        .unwrap();
    let key = ValueIndexKey::from("id");
    let count = |engine: &crate::Engine| {
        engine
            .sql("SELECT count(*) FROM kept WHERE id BETWEEN 10 AND 19", &[])
            .unwrap()
            .value_at(0, 0)
            .cloned()
    };
    assert_eq!(count(&engine), Some(Value::Int(10)));
    let has_index = |engine: &crate::Engine| {
        engine
            .try_table("kept")
            .unwrap()
            .unwrap()
            .value_indexes
            .read()
            .contains_key(&key)
    };
    assert!(has_index(&engine));
    // Only this engine writes its tables, and each write maintained the index, so a new data generation keeps it.
    engine.sql("DELETE FROM kept WHERE id = 12", &[]).unwrap();
    engine.synchronize_table_data().unwrap();
    assert!(has_index(&engine));
    assert_eq!(count(&engine), Some(Value::Int(9)));
    // A rolled-back write restores the index of the snapshot instead of keeping the undone posting.
    engine
        .sql("BEGIN; INSERT INTO kept VALUES (12, 12)", &[])
        .unwrap();
    assert_eq!(count(&engine), Some(Value::Int(10)));
    engine.sql("ROLLBACK", &[]).unwrap();
    engine.synchronize_table_data().unwrap();
    assert_eq!(count(&engine), Some(Value::Int(9)));
}

fn ids(engine: &crate::Engine, sql: &str) -> Vec<Value> {
    engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| row["id"].clone())
        .collect()
}

fn assert_lookups(engine: &crate::Engine, expected: &[(i64, &[i64])]) {
    for (category, documents) in expected {
        assert_eq!(
            ids(
                engine,
                &format!("SELECT id FROM items WHERE category = {category}")
            ),
            documents
                .iter()
                .map(|id| Value::Int(*id))
                .collect::<Vec<_>>(),
            "category {category}"
        );
    }
}

#[test]
fn rollbacks_leave_the_rows_they_undo_out_of_every_lookup() {
    let directory = tempfile::tempdir().unwrap();
    let engine = crate::Engine::open(&directory.path().join("rolled-back-lookups.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, category INTEGER); CREATE INDEX items_category ON items (category); INSERT INTO items VALUES (1, 10), (2, 20), (3, 30)",
            &[],
        )
        .unwrap();
    // The lookups build the in-memory indexes before the transactions change their rows.
    assert_lookups(&engine, &[(10, &[1]), (20, &[2]), (30, &[3])]);
    engine
        .sql(
            "BEGIN; UPDATE items SET category = 21 WHERE id = 2; SAVEPOINT before_rows; DELETE FROM items WHERE id = 3; INSERT INTO items VALUES (4, 40)",
            &[],
        )
        .unwrap();
    assert_lookups(&engine, &[(21, &[2]), (30, &[]), (40, &[4])]);
    engine.sql("ROLLBACK TO before_rows", &[]).unwrap();
    assert_lookups(&engine, &[(20, &[]), (21, &[2]), (30, &[3]), (40, &[])]);
    // A failed statement leaves out the rows it wrote before it failed.
    assert_eq!(
        engine
            .sql("INSERT INTO items VALUES (5, 50), (1, 11)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    engine.sql("ROLLBACK TO before_rows", &[]).unwrap();
    assert_lookups(&engine, &[(50, &[]), (11, &[]), (10, &[1])]);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_lookups(&engine, &[(20, &[2]), (21, &[]), (30, &[3]), (40, &[])]);
    // So does a statement that fails outside a transaction.
    assert_eq!(
        engine
            .sql("INSERT INTO items VALUES (6, 60), (1, 11)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    assert_lookups(&engine, &[(60, &[]), (11, &[]), (10, &[1])]);
}
