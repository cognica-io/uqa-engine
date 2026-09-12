//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use parking_lot::Mutex;
use tempfile::tempdir;
use uqa_core::DocId;
use uqa_storage::document_store::Document;
use uqa_storage::{
    DocumentMetadata, DocumentStore, MemoryDocumentStore, StorageBackendResult, StoredDocument,
};

use crate::Engine;

type ProjectionLog = Arc<Mutex<Vec<Vec<String>>>>;
type DocumentBatchLog = Arc<Mutex<Vec<Vec<DocId>>>>;

struct ProjectionRecordingStore {
    inner: Box<dyn DocumentStore>,
    projections: ProjectionLog,
    document_batches: DocumentBatchLog,
}

impl DocumentStore for ProjectionRecordingStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        self.inner.put(doc_id, document)
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        self.inner.get(doc_id)
    }

    fn put_stored(&mut self, doc_id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        self.inner.put_stored(doc_id, document)
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.inner.get_stored(doc_id)
    }

    fn get_stored_many(
        &self,
        doc_ids: &[DocId],
    ) -> StorageBackendResult<std::collections::BTreeMap<DocId, StoredDocument>> {
        self.inner.get_stored_many(doc_ids)
    }

    fn get_metadata(&self, doc_id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.inner.get_metadata(doc_id)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.inner.delete(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.inner.clear()
    }

    fn for_each_fields_multi_ref_with_presence(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&uqa_core::Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.projections
            .lock()
            .push(fields.iter().map(|field| (*field).to_string()).collect());
        self.document_batches.lock().push(doc_ids.to_vec());
        self.inner
            .for_each_fields_multi_ref_with_presence(doc_ids, fields, visitor)
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.inner.doc_ids()
    }

    fn len(&self) -> StorageBackendResult<usize> {
        self.inner.len()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        self.inner.snapshot()
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
        self.inner.writable_snapshot()
    }
}

fn install_projection_recorder(
    engine: &Engine,
    table_name: &str,
) -> (ProjectionLog, DocumentBatchLog) {
    let table = engine.require_table(table_name).unwrap();
    let projections = Arc::new(Mutex::new(Vec::new()));
    let document_batches = Arc::new(Mutex::new(Vec::new()));
    let mut store = table.document_store.write();
    let inner = std::mem::replace(&mut *store, Box::new(MemoryDocumentStore::new()));
    *store = Box::new(ProjectionRecordingStore {
        inner,
        projections: Arc::clone(&projections),
        document_batches: Arc::clone(&document_batches),
    });
    (projections, document_batches)
}

fn assert_no_retrieval_fields(projections: &[Vec<String>]) {
    assert!(
        !projections.is_empty(),
        "query must fetch its projected rows"
    );
    for projection in projections {
        assert!(!projection.iter().any(|field| field == "body"));
        assert!(!projection.iter().any(|field| field == "embedding"));
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "covers projection pruning cases")]
fn accelerated_retrieval_materializes_only_relational_dependencies() {
    let directory = tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("projection.sqlite3")).unwrap();
    engine
            .sql(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2), marker TEXT)",
                &[],
            )
            .unwrap();
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, body, embedding, marker) VALUES \
                 (1, 'alpha beta', ARRAY[1.0, 0.0], 'keep'), \
                 (2, 'alpha gamma', ARRAY[0.9, 0.1], 'keep'), \
                 (3, 'delta', ARRAY[0.0, 1.0], 'drop')",
            &[],
        )
        .unwrap();
    let (projections, document_batches) = install_projection_recorder(&engine, "docs");
    let hybrid = "pool_positive_evidence(\
            bayesian_match(body, 'alpha'), \
            calibrated_vector_match(embedding, ARRAY[1.0, 0.0], 3), \
            alpha => 0.5)";

    let result = engine
        .sql(
            &format!(
                "SELECT id, _score FROM docs WHERE {hybrid} \
                     ORDER BY _score DESC, id ASC LIMIT 2"
            ),
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    {
        let recorded = projections.lock();
        assert_no_retrieval_fields(&recorded);
        assert!(recorded
            .iter()
            .all(|projection| projection == &["id".to_string()]));
    }
    assert_eq!(
        document_batches.lock().iter().map(Vec::len).sum::<usize>(),
        2
    );

    let (_, document_batches) = install_projection_recorder(&engine, "docs");
    let second = engine
        .sql(
            &format!(
                "SELECT id, _score FROM docs WHERE {hybrid} \
                     ORDER BY _score DESC, id ASC LIMIT 1 OFFSET 1"
            ),
            &[],
        )
        .unwrap();
    assert_eq!(second.rows.len(), 1);
    assert_eq!(second.rows[0].get("id"), result.rows[1].get("id"));
    assert_eq!(
        document_batches.lock().iter().map(Vec::len).sum::<usize>(),
        2
    );

    let (projections, _) = install_projection_recorder(&engine, "docs");
    let projected = engine
        .sql(
            &format!(
                "SELECT id, embedding, _score FROM docs WHERE {hybrid} \
                     ORDER BY _score DESC, id ASC LIMIT 2"
            ),
            &[],
        )
        .unwrap();
    assert_eq!(projected.rows.len(), 2);
    {
        let recorded = projections.lock();
        assert!(!recorded.is_empty());
        assert!(recorded.iter().all(|projection| {
            projection.iter().any(|field| field == "id")
                && projection.iter().any(|field| field == "embedding")
                && !projection.iter().any(|field| field == "body")
        }));
    }

    let (projections, _) = install_projection_recorder(&engine, "docs");
    let filtered = engine
        .sql("SELECT id FROM docs WHERE marker = 'keep' ORDER BY id", &[])
        .unwrap();
    assert_eq!(filtered.rows.len(), 2);
    {
        let recorded = projections.lock();
        assert!(!recorded.is_empty());
        assert!(recorded.iter().all(|projection| {
            projection.iter().any(|field| field == "id")
                && projection.iter().any(|field| field == "marker")
        }));
    }

    let (projections, _) = install_projection_recorder(&engine, "docs");
    let facets = engine
        .sql(
            &format!("SELECT uqa_facets(marker) FROM docs WHERE {hybrid}"),
            &[],
        )
        .unwrap();
    assert!(!facets.rows.is_empty());
    let recorded = projections.lock();
    assert_no_retrieval_fields(&recorded);
    assert!(recorded
        .iter()
        .all(|projection| { projection == &["marker".to_string()] }));
}

#[test]
fn score_cutoff_leaves_secondary_ordering_exact_across_ties() {
    let directory = tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("score-ties.sqlite3")).unwrap();
    engine
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, body) VALUES \
                 (1, 'alpha'), (2, 'alpha'), (3, 'alpha')",
            &[],
        )
        .unwrap();
    let (_, document_batches) = install_projection_recorder(&engine, "docs");

    let result = engine
        .sql(
            "SELECT id, _score FROM docs WHERE bayesian_match(body, 'alpha') \
                 ORDER BY _score DESC, id DESC LIMIT 2",
            &[],
        )
        .unwrap();

    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        vec![
            Some(&uqa_core::Value::Int(3)),
            Some(&uqa_core::Value::Int(2))
        ]
    );
    assert_eq!(
        document_batches.lock().iter().map(Vec::len).sum::<usize>(),
        3,
        "all rows tied at the cutoff score must reach secondary ordering"
    );
}
