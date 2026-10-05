//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage_sqlite::{Catalog, SQLiteStorageBackend};

mod legacy_vectors;
mod retained;

#[derive(Clone)]
struct StoreWithMissingDocId {
    docs: BTreeMap<DocId, StoredDocument>,
    missing_doc_id: DocId,
    read_snapshot_calls: Arc<std::sync::atomic::AtomicUsize>,
    writable_snapshot_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl StoreWithMissingDocId {
    fn from_table(engine: &Engine, table: &str, missing_doc_id: DocId) -> Self {
        let table = engine.table(table).unwrap().expect("table");
        let store = table.document_store.read();
        let doc_ids = store.doc_ids().unwrap();
        let docs = store.get_stored_many(&doc_ids).unwrap();
        Self {
            docs,
            missing_doc_id,
            read_snapshot_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            writable_snapshot_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

impl DocumentStore for StoreWithMissingDocId {
    fn put_stored(&mut self, doc_id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.docs.insert(doc_id, document);
        Ok(())
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok(self.docs.get(&doc_id).cloned())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.docs.remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.docs.clear();
        Ok(())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        let mut ids = vec![self.missing_doc_id];
        ids.extend(self.docs.keys().copied());
        Ok(ids)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.docs.len() + 1)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.read_snapshot_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        self.writable_snapshot_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Box::new(self.clone()))
    }
}

#[test]
fn transaction_snapshot_captures_one_writable_copy_without_probe_clone() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let store = StoreWithMissingDocId::from_table(&eng, "docs", 99);
    let read_calls = store.read_snapshot_calls.clone();
    let writable_calls = store.writable_snapshot_calls.clone();
    {
        let table = eng.table("docs").unwrap().expect("table");
        *table.document_store.write() = Box::new(store);
    }

    eng.begin().unwrap();
    eng.commit().unwrap();

    assert_eq!(read_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(writable_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

type SnapshotErrorFactory = fn() -> StorageBackendError;

#[derive(Clone)]
struct PortalSnapshotProbeStore {
    docs: Arc<BTreeMap<DocId, StoredDocument>>,
    snapshot_error: Option<SnapshotErrorFactory>,
    snapshot_calls: Arc<std::sync::atomic::AtomicUsize>,
    row_reads: Arc<std::sync::atomic::AtomicUsize>,
    doc_id_calls: Arc<std::sync::atomic::AtomicUsize>,
    catalog_was_unlocked: Arc<std::sync::atomic::AtomicBool>,
    tables: std::sync::Weak<parking_lot::RwLock<BTreeMap<RelationIdentity, Arc<TableState>>>>,
}

impl PortalSnapshotProbeStore {
    fn from_table(engine: &Engine, table: &str) -> Self {
        let table = engine.table(table).unwrap().expect("table");
        let store = table.document_store.read();
        let doc_ids = store.doc_ids().unwrap();
        let docs = store.get_stored_many(&doc_ids).unwrap();
        Self {
            docs: Arc::new(docs),
            snapshot_error: None,
            snapshot_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            row_reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            doc_id_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            catalog_was_unlocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tables: Arc::downgrade(&engine.storage.tables),
        }
    }
}

impl DocumentStore for PortalSnapshotProbeStore {
    fn field_presence_controlled(
        &self,
        ids: &[DocId],
        fields: &[&str],
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::BudgetedVec<bool>> {
        control.check()?;
        let mut present = uqa_core::memory::BudgetedVec::new(control.memory());
        for id in ids {
            for field in fields {
                control.check()?;
                present.push(
                    self.docs
                        .get(id)
                        .is_some_and(|row| row.fields().contains_key(*field)),
                )?;
            }
        }
        Ok(present)
    }

    fn put_stored(&mut self, doc_id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        Arc::make_mut(&mut self.docs).insert(doc_id, document);
        Ok(())
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.row_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.docs.get(&doc_id).cloned())
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.docs.contains_key(&doc_id))
    }

    fn get_metadata(&self, doc_id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        Ok(self.docs.get(&doc_id).map(StoredDocument::metadata))
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        Ok(self
            .docs
            .get(&doc_id)
            .and_then(|row| row.fields().get(field))
            .cloned())
    }

    fn get_fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        Ok(ids
            .iter()
            .filter_map(|id| {
                self.docs.get(id).map(|row| {
                    let values = fields
                        .iter()
                        .map(|field| row.fields().get(*field).cloned().unwrap_or(Value::Null))
                        .collect();
                    (*id, values)
                })
            })
            .collect())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        Arc::make_mut(&mut self.docs).remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.docs = Arc::default();
        Ok(())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.doc_id_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.docs.keys().copied().collect())
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.docs.len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.snapshot_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.catalog_was_unlocked.store(
            self.tables
                .upgrade()
                .is_some_and(|tables| tables.try_write().is_some()),
            std::sync::atomic::Ordering::Relaxed,
        );

        if let Some(error) = self.snapshot_error {
            return Err(error());
        }
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        Ok(Box::new(self.clone()))
    }
}

#[test]
fn parameter_expression_index_probes_do_not_enumerate_the_document_store() {
    use std::sync::atomic::Ordering;
    let engine = Engine::new();
    engine.sql("CREATE TABLE parameter_index (id INTEGER, qty INTEGER); CREATE INDEX parameter_qty ON parameter_index (qty)", &[]).unwrap();
    let rows = (1..=64)
        .map(|qty| format!("({}, {qty})", qty * 10))
        .collect::<Vec<_>>()
        .join(", ");
    engine
        .sql(&format!("INSERT INTO parameter_index VALUES {rows}"), &[])
        .unwrap();
    engine
        .sql("SELECT id FROM parameter_index WHERE qty = 1", &[])
        .unwrap();
    let probe = PortalSnapshotProbeStore::from_table(&engine, "parameter_index");
    let enumerations = Arc::clone(&probe.doc_id_calls);
    *engine
        .table("parameter_index")
        .unwrap()
        .unwrap()
        .document_store
        .write() = Box::new(probe);
    for parameter in [3, 31, 999] {
        let result = engine
            .sql(
                "SELECT id FROM parameter_index WHERE qty = $1 + 1",
                &[SQLParam::scalar(Value::Int(parameter))],
            )
            .unwrap();
        let expected = if parameter == 999 {
            vec![]
        } else {
            vec![Value::Int((parameter + 1) * 10)]
        };
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            enumerations.load(Ordering::Relaxed),
            0,
            "parameter lookup enumerated stored rows"
        );
    }
    engine.sql("SET plan_cache_mode = force_generic_plan; PREPARE parameter_probe(integer) AS SELECT id FROM parameter_index WHERE qty = $1 + 1", &[]).unwrap();
    for parameter in [7, 31] {
        let result = engine
            .sql(&format!("EXECUTE parameter_probe({parameter})"), &[])
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int((parameter + 1) * 10));
        assert_eq!(
            enumerations.load(Ordering::Relaxed),
            0,
            "generic plan enumerated stored rows"
        );
    }
}

#[test]
fn cursor_snapshots_only_referenced_tables_without_holding_the_catalog_lock() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE portal_used (id INTEGER, embedding VECTOR(2)); CREATE TABLE portal_right (id INTEGER, archived_embedding VECTOR(2)); CREATE TABLE portal_unrelated (id INTEGER); CREATE SCHEMA portal_shadow; CREATE TABLE portal_shadow.portal_used (id INTEGER, embedding VECTOR(2)); INSERT INTO portal_used VALUES (1, ARRAY[1.0, 0.0]); INSERT INTO portal_right VALUES (101, ARRAY[1.0, 0.0]); INSERT INTO portal_unrelated VALUES (2); INSERT INTO portal_shadow.portal_used VALUES (10, ARRAY[1.0, 0.0]), (11, ARRAY[0.9, 0.1])",
        &[],
    )
    .unwrap();
    let used = PortalSnapshotProbeStore::from_table(&eng, "portal_used");
    let right = PortalSnapshotProbeStore::from_table(&eng, "portal_right");
    let unrelated = PortalSnapshotProbeStore::from_table(&eng, "portal_unrelated");
    let used_calls = Arc::clone(&used.snapshot_calls);
    let right_calls = Arc::clone(&right.snapshot_calls);
    let unrelated_calls = Arc::clone(&unrelated.snapshot_calls);
    let used_catalog_was_unlocked = Arc::clone(&used.catalog_was_unlocked);
    let document_reads = [
        Arc::clone(&used.doc_id_calls),
        Arc::clone(&used.row_reads),
        Arc::clone(&right.doc_id_calls),
        Arc::clone(&right.row_reads),
        Arc::clone(&unrelated.doc_id_calls),
        Arc::clone(&unrelated.row_reads),
    ];
    let read_counts = || {
        document_reads
            .each_ref()
            .map(|calls| calls.load(std::sync::atomic::Ordering::Relaxed))
    };
    *eng.table("portal_used")
        .unwrap()
        .expect("portal_used")
        .document_store
        .write() = Box::new(used);
    *eng.table("portal_unrelated")
        .unwrap()
        .expect("portal_unrelated")
        .document_store
        .write() = Box::new(unrelated);
    *eng.table("portal_right")
        .unwrap()
        .expect("portal_right")
        .document_store
        .write() = Box::new(right);

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("DECLARE constant_cursor CURSOR FOR SELECT 1", &[])
        .unwrap();
    assert_eq!(used_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(right_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(
        unrelated_calls.load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    let before = read_counts();
    eng.sql(
        "DECLARE table_cursor CURSOR FOR SELECT id FROM portal_used",
        &[],
    )
    .unwrap();
    assert_eq!(used_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(right_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(
        unrelated_calls.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(used_catalog_was_unlocked.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(read_counts(), before);
    assert_eq!(
        eng.sql("FETCH ALL FROM table_cursor", &[])
            .unwrap()
            .rows
            .len(),
        1
    );

    let before = read_counts();
    eng.sql(
        "DECLARE operator_cursor CURSOR FOR SELECT left_doc_id FROM vector_similarity_join(portal_used, knn_match(embedding, ARRAY[1.0, 0.0], 1), portal_right, knn_match(archived_embedding, ARRAY[1.0, 0.0], 1), 0.8)",
        &[],
    )
    .unwrap();
    assert_eq!(used_calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(right_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        unrelated_calls.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(read_counts(), before);
    eng.sql("SET search_path = portal_shadow, public", &[])
        .unwrap();
    assert_eq!(
        eng.sql("FETCH ALL FROM operator_cursor", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    eng.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn portal_temporal_inputs_keep_the_transaction_clock_through_fetch_and_hold() {
    fn verify(engine: &Engine) {
        let _caller_clock = uqa_sql::expr::TransactionClockScope::enter(17);
        let started_at = 90_123_456_789;
        let expected = Value::Temporal(uqa_core::TemporalValue::Timestamp { micros: started_at });
        for (holdable, fetch_before_commit) in [(false, true), (true, false), (true, true)] {
            engine.sql("BEGIN", &[]).unwrap();
            engine.session.transactions.lock()[0].started_at_micros = started_at;
            let hold = if holdable { "WITH HOLD" } else { "" };
            engine
                .sql(
                    &format!("DECLARE temporal_clock SCROLL CURSOR {hold} FOR SELECT 'now'::text::timestamp AS input_clock, now()::timestamp AS transaction_clock FROM (VALUES (1), (2)) AS source(id)"),
                    &[],
                )
                .unwrap();
            let verify_rows = |result: &uqa_sql::SQLResult, count| {
                assert_eq!(result.rows.len(), count);
                for row in &result.rows {
                    assert_eq!(row["input_clock"], expected);
                    assert_eq!(row["transaction_clock"], expected);
                }
                assert_eq!(uqa_sql::expr::transaction_clock_micros(), Some(17));
            };
            if fetch_before_commit {
                verify_rows(
                    &engine.sql("FETCH NEXT FROM temporal_clock", &[]).unwrap(),
                    1,
                );
            }
            if holdable {
                engine.sql("COMMIT", &[]).unwrap();
            }
            verify_rows(
                &engine.sql("FETCH ALL FROM temporal_clock", &[]).unwrap(),
                if fetch_before_commit { 1 } else { 2 },
            );
            engine.sql("CLOSE temporal_clock", &[]).unwrap();
            if !holdable {
                engine.sql("ROLLBACK", &[]).unwrap();
            }
        }
    }

    verify(&Engine::new());
    let directory = tempfile::tempdir().unwrap();
    verify(&Engine::open(&directory.path().join("portal-clocks.db")).unwrap());
}

#[test]
fn sql_update_reports_stale_document_ids() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE docs (
           id INTEGER PRIMARY KEY,
           status TEXT,
           title TEXT,
           content TEXT
         )",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX docs_fts ON docs USING gin (title, content)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO docs (id, status, title, content)
         VALUES (1, 'queued', 'Runtime search', 'old content'),
                (2, 'indexed', 'Other', 'other content')",
        &[],
    )
    .unwrap();
    {
        let table = eng.table("docs").unwrap().expect("table");
        *table.document_store.write() =
            Box::new(StoreWithMissingDocId::from_table(&eng, "docs", 99));
    }

    let error = eng
        .sql(
            "UPDATE docs
                SET content = 'updated content',
                    status = 'indexed'
              WHERE id = 1 AND status = 'queued'",
            &[],
        )
        .expect_err("a stale index candidate must not be treated as no matching row");

    assert!(
        error.to_string().contains(
            "candidate 99 is missing from the document-field snapshot for table `public.docs`"
        ),
        "unexpected error: {error}"
    );
    let doc = eng.get_document("docs", 1).unwrap().unwrap();
    assert_eq!(doc.get("content"), Some(&s("old content")));
    assert_eq!(doc.get("status"), Some(&s("queued")));
}

/// Document store whose next `fail_budget` put calls fail. Used to
/// prove that an ON CONFLICT DO UPDATE whose delete succeeded but
/// whose re-insert failed surfaces the error and rolls back instead
/// of committing the row away (the Maek `global_config` loss shape).
#[derive(Clone)]
struct FailingPutStore {
    docs: BTreeMap<DocId, StoredDocument>,
    fail_budget: Arc<std::sync::atomic::AtomicUsize>,
}

impl FailingPutStore {
    fn from_table(engine: &Engine, table: &str, fail_budget: usize) -> Self {
        let table = engine.table(table).unwrap().expect("table");
        let store = table.document_store.read();
        let doc_ids = store.doc_ids().unwrap();
        let docs = store.get_stored_many(&doc_ids).unwrap();
        Self {
            docs,
            fail_budget: Arc::new(std::sync::atomic::AtomicUsize::new(fail_budget)),
        }
    }
}

impl DocumentStore for FailingPutStore {
    fn put_stored(&mut self, doc_id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        let remaining = self.fail_budget.load(Ordering::SeqCst);
        if remaining > 0 {
            self.fail_budget.store(remaining - 1, Ordering::SeqCst);
            return Err(StorageBackendError::Other(
                "injected put failure".to_string(),
            ));
        }
        self.docs.insert(doc_id, document);
        Ok(())
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        Ok(self.docs.get(&doc_id).cloned())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.docs.remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.docs.clear();
        Ok(())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        Ok(self.docs.keys().copied().collect())
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.docs.len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        Ok(Box::new(self.clone()))
    }
}

#[test]
fn upsert_reinsert_failure_rolls_back_instead_of_losing_the_row() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE engine_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO engine_meta (key, value) VALUES ('global_config', 'v1')",
        &[],
    )
    .unwrap();
    {
        let table = eng.table("engine_meta").unwrap().expect("table");
        *table.document_store.write() =
            Box::new(FailingPutStore::from_table(&eng, "engine_meta", 1));
    }

    let err = eng
        .sql(
            "INSERT INTO engine_meta (key, value) VALUES ('global_config', 'v2') \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[],
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("document store write failed"),
        "unexpected error: {err}"
    );

    // The row must survive with its previous value: the failed
    // rewrite has to roll back, not commit its delete half.
    let result = eng
        .sql(
            "SELECT value FROM engine_meta WHERE key = 'global_config'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("value"), Some(&s("v1")));

    // With the fault budget exhausted the same upsert succeeds.
    eng.sql(
        "INSERT INTO engine_meta (key, value) VALUES ('global_config', 'v2') \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        &[],
    )
    .unwrap();
    let result = eng
        .sql(
            "SELECT value FROM engine_meta WHERE key = 'global_config'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0].get("value"), Some(&s("v2")));
}

#[test]
fn persistent_engine_restores_through_facade_traits() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("facade.db");
    let conn = ManagedConnection::open(&db).unwrap();
    let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(conn.clone()).unwrap());
    let backend: Arc<dyn PersistentStorageBackend> =
        Arc::new(SQLiteStorageBackend::new(conn.clone()));

    {
        let eng = Engine::from_persistent_backends(catalog.clone(), backend.clone()).unwrap();
        eng.create_default_table("docs", vec!["title".into()])
            .unwrap();
        eng.add_document("docs", 1, doc([("title", s("hello facade"))]))
            .unwrap();
    }

    let reopened = Engine::from_persistent_backends(catalog, backend).unwrap();
    assert_eq!(reopened.document_count("docs").unwrap(), 1);
    let hits = reopened
        .search("docs", "title", "facade", &ScoringMode::default(), 10)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
}
