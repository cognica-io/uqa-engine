//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_storage::document_store::{Document, DocumentStore};

#[derive(Clone)]
struct MissingProjectionStore;

impl DocumentStore for MissingProjectionStore {
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

fn ids(list: &PostingList) -> Vec<DocId> {
    list.entries().iter().map(|e| e.doc_id).collect()
}

#[test]
fn build_scan_equals_and_ranges() {
    let index = ColumnValueIndex::build(
        "qty",
        vec![
            (1, Value::Int(10)),
            (2, Value::Int(20)),
            (3, Value::Int(20)),
            (4, Value::Null),
            (5, Value::Int(30)),
        ]
        .into_iter(),
    );
    assert_eq!(
        ids(&index.scan(&Predicate::Equals(Value::Int(20))).unwrap()),
        vec![2, 3]
    );
    assert_eq!(
        ids(&index.scan(&Predicate::GreaterThan(Value::Int(10))).unwrap()),
        vec![2, 3, 5]
    );
    assert_eq!(
        ids(&index
            .scan(&Predicate::Between {
                low: Value::Int(10),
                high: Value::Int(20),
            })
            .unwrap()),
        vec![1, 2, 3]
    );
    assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()), vec![4]);
    assert_eq!(
        ids(&index.scan(&Predicate::IsNotNull).unwrap()),
        vec![1, 2, 3, 5]
    );
    assert!(index.scan(&Predicate::NotEquals(Value::Int(10))).is_none());
}

#[test]
fn incremental_insert_remove_tracks_nulls() {
    let mut index = ColumnValueIndex::build("qty", std::iter::empty());
    index.insert(7, &Value::Int(1));
    index.insert(8, &Value::Null);
    assert_eq!(
        ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()),
        vec![7]
    );
    assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()), vec![8]);
    index.remove(7, &Value::Int(1));
    index.remove(8, &Value::Null);
    assert!(ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()).is_empty());
    assert!(ids(&index.scan(&Predicate::IsNull).unwrap()).is_empty());
}

#[test]
fn temporal_and_nan_guards_refuse_acceleration() {
    let temporal = uqa_core::TemporalValue::parse_date("2024-01-01").unwrap();
    let index = ColumnValueIndex::build(
        "ts",
        vec![(1, Value::Temporal(temporal.clone()))].into_iter(),
    );
    assert!(index
        .scan(&Predicate::Equals(Value::Str("2024-01-01".into())))
        .is_none());

    let numeric = ColumnValueIndex::build("f", vec![(1, Value::Float(1.0))].into_iter());
    assert!(numeric
        .scan(&Predicate::Equals(Value::Float(f64::NAN)))
        .is_none());
    assert!(numeric
        .scan(&Predicate::Equals(Value::Temporal(temporal)))
        .is_none());
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
        .ensure_value_index("projection_gap", "id")
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
    backend.drop_btree_index("public.items", "id").unwrap();
    let table = engine.try_table("items").unwrap().unwrap();
    crate::Engine::value_indexes_clear(&table);

    let result = engine
        .sql("SELECT id FROM items WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert!(backend
        .load_btree_index("public.items", "id")
        .unwrap()
        .is_none());
    assert!(engine
        .try_table("items")
        .unwrap()
        .unwrap()
        .value_indexes
        .read()
        .contains_key("id"));

    // Rollback recovery clears hot indexes before hydrating them again;
    // a missing durable marker must remain a memory-only cache miss there
    // too, rather than silently turning rollback into a new write.
    engine.reload_persistent_value_indexes().unwrap();
    assert!(backend
        .load_btree_index("public.items", "id")
        .unwrap()
        .is_none());
    assert!(engine
        .try_table("items")
        .unwrap()
        .unwrap()
        .value_indexes
        .read()
        .contains_key("id"));

    // The explicit persistence path must not mistake the hot memory cache
    // for a durable marker.
    engine.ensure_persistent_value_index("items", "id").unwrap();
    assert_eq!(
        backend
            .load_btree_index("public.items", "id")
            .unwrap()
            .unwrap(),
        vec![(1, Value::Int(1))]
    );
}

#[test]
fn open_repair_discards_raw_alias_and_rebuilds_canonical_index() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("repair-btree.db");
    let engine = crate::Engine::open(&database).unwrap();
    engine
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO items (id) VALUES (1)", &[])
        .unwrap();
    let backend = engine.storage.backend.as_ref().unwrap().clone();
    backend.drop_btree_index("public.items", "id").unwrap();
    backend
        .replace_btree_index("public.items", "obsolete", &[(1, Value::Int(888))])
        .unwrap();
    // Bypass the v21 guard only to inject a pre-v17 unqualified alias that
    // a current engine would never create, then restore the guard before
    // exercising open repair.
    let raw = rusqlite::Connection::open(&database).unwrap();
    raw.execute("DROP TRIGGER _btree_entries_document_insert", [])
        .unwrap();
    backend
        .replace_btree_index("items", "id", &[(1, Value::Int(999))])
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

    assert!(backend.load_btree_index("items", "id").unwrap().is_none());
    assert!(backend
        .load_btree_index("public.items", "obsolete")
        .unwrap()
        .is_none());
    assert_eq!(
        backend
            .load_btree_index("public.items", "id")
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
