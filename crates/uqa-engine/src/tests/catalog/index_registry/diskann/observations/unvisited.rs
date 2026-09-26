//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use std::{collections::BTreeSet, path::Path, sync::atomic::Ordering};
use uqa_core::DocId;
use uqa_storage::{
    diskann_index::{
        format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
        pages::DiskANNPageSource,
        DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNIndexOptions, DiskANNQueryRead,
        RetainedDiskANNIndex,
    },
    key_value::KeyValueDiskANNCanonical,
    mvcc::VersionedSessionOptions,
    vector_index::DiskANNIndexParams,
    CatalogIndexRow, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore, StorageBackendResult,
    VectorIndex,
};
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteDiskANNCanonical, SQLiteStorageBackend,
};

enum Source {
    Native(ManagedConnection),
    KeyValue(Arc<dyn KeyValueStore>),
}

type Visits = Arc<Mutex<BTreeSet<DocId>>>;

fn open(path: &Path, provider: usize) -> (Engine, Source) {
    if provider == 0 {
        let connection = ManagedConnection::open(path).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let engine = Engine::from_persistent_backends(
            Arc::new(Catalog::open(connection.clone()).unwrap()),
            Arc::new(SQLiteStorageBackend::new(connection.clone())),
        )
        .unwrap();
        return (engine, Source::Native(connection));
    }
    let store: Arc<dyn KeyValueStore> = if provider == 1 {
        Arc::new(uqa_storage_sqlite::SQLiteKeyValueStore::open(path).unwrap())
    } else {
        Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap().store())
    };
    let engine = Engine::from_persistent_backends(
        Arc::new(KeyValueCatalog::new(store.clone())),
        Arc::new(KeyValueStorageBackend::new(store.clone())),
    )
    .unwrap();
    (engine, Source::KeyValue(store))
}

impl Source {
    fn retain(
        &self,
        row: &CatalogIndexRow,
        control: &StorageReadControl,
    ) -> (RetainedDiskANNIndex<TracedCanonical>, Visits) {
        let resolver = uqa_execution::catalog::index::diskann::DiskANNIndexIdentityResolver;
        let (canonical, physical): (
            Box<dyn DiskANNQueryRead + Send + Sync>,
            Arc<dyn DiskANNPageSource>,
        ) = match self {
            Self::Native(connection) => {
                let canonical = SQLiteDiskANNCanonical::new(
                    connection.clone(),
                    &row.table_name,
                    "embedding",
                    2,
                )
                .unwrap()
                .retain_for_index(&row.relation, control)
                .unwrap();
                let physical = canonical
                    .selected_source(&resolver, control)
                    .unwrap()
                    .unwrap();
                (Box::new(canonical), physical)
            }
            Self::KeyValue(store) => {
                let canonical =
                    KeyValueDiskANNCanonical::new(store.clone(), &row.table_name, "embedding", 2)
                        .unwrap()
                        .retain_for_index(&row.relation, control)
                        .unwrap();
                let physical = canonical
                    .selected_source(&resolver, control)
                    .unwrap()
                    .unwrap();
                (Box::new(canonical), physical)
            }
        };
        let parameters = DiskANNIndexParams::from_catalog_map(
            2,
            &serde_json::from_str(&row.parameters_json).unwrap(),
        )
        .unwrap();
        let visits = Visits::default();
        let index = RetainedDiskANNIndex::open(
            TracedCanonical {
                canonical,
                visits: visits.clone(),
            },
            physical,
            parameters,
            DiskANNIndexOptions::for_parameters(parameters).read,
            control,
        )
        .unwrap();
        (index, visits)
    }
}

struct TracedCanonical {
    canonical: Box<dyn DiskANNQueryRead + Send + Sync>,
    visits: Visits,
}

impl DiskANNCanonicalRead for TracedCanonical {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.canonical.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.canonical.dimensions()
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.canonical.next_document_after(after, control)
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.canonical.origin(document, control)
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.visits.lock().insert(document);
        self.canonical.visit_document(document, control, visit)
    }
}

impl DiskANNQueryRead for TracedCanonical {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.canonical.document_origin(document, control)
    }
    fn next_change_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.canonical.next_change_after(after, control)
    }
}

#[test]
fn diskann_sql_serializable_unvisited_candidates_conflict_in_both_commit_orders() {
    for provider in 0..3 {
        for (observe, cached) in [(true, false), (true, true), (false, true)] {
            for writer_first in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let (first, source) = open(&directory.path().join("unvisited.db"), provider);
                let second = first.new_session().unwrap();
                for engine in [&first, &second] {
                    engine.release_automatic_statistics_client();
                    engine
                        .session
                        .statistics_worker
                        .store(true, Ordering::Release);
                }
                sql(&first, "CREATE TABLE t(v int); INSERT INTO t VALUES(1); CREATE TABLE diskann_docs(id int PRIMARY KEY, embedding vector(2)); INSERT INTO diskann_docs SELECT i, ARRAY[0.0,1.0] FROM generate_series(1,8) AS s(i); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(max_degree=2,build_list_size=4,search_list_size=1,beam_width=1,pq_bytes=1)");
                let definition = first.catalog_index("diskann_idx").unwrap().unwrap();
                sql(&first, "BEGIN ISOLATION LEVEL SERIALIZABLE");
                sql(&second, "BEGIN ISOLATION LEVEL SERIALIZABLE");
                first.prepare_serializable_transaction_snapshot().unwrap();
                let table = first.try_table("diskann_docs").unwrap().unwrap();
                let documents = table.document_store.read().doc_ids().unwrap();
                assert_eq!(documents.len(), 8);
                let read = first
                    .serializable_table_state_read(&table)
                    .unwrap()
                    .unwrap();
                let control = first.query_retention_control().unwrap();
                let (retained, visits) = source.retain(&definition, &control);
                assert_eq!(retained.manifest().input().nodes, 8);
                // Trace the production canonical reader; the graph, pages and original session remain real provider-owned resources.
                if cached {
                    retained.search_knn(&[1.0, 0.0], 1).unwrap();
                    visits.lock().clear();
                }
                let observed = uqa_execution::serializable::vector::observe_snapshot(
                    observe.then_some(&read),
                    &table.columns.read(),
                    "embedding",
                    Arc::new(retained),
                )
                .unwrap()
                .snapshot()
                .unwrap()
                .snapshot()
                .unwrap();
                let found = observed
                    .search_knn_with_control(&[1.0, 0.0], 1, &control)
                    .unwrap();
                assert_eq!(found.len(), 1);
                assert_eq!(found.iter().next().unwrap().payload.score, 0.0);
                let unseen = documents
                    .into_iter()
                    .find(|doc| !visits.lock().contains(doc))
                    .expect("narrow ANN must leave an unexpanded canonical candidate");
                let document = table.document_store.read().get(unseen).unwrap().unwrap();
                let Value::Int(id) = document["id"] else {
                    panic!("integer identity");
                };
                sql(&second, "SELECT v FROM t");
                sql(&first, "UPDATE t SET v=2");
                sql(
                    &second,
                    &format!("UPDATE diskann_docs SET embedding=ARRAY[1.0,0.0] WHERE id={id}"),
                );
                let changed = sql(&second, "SELECT id, _score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)");
                assert_eq!(changed.rows.len(), 1);
                assert_eq!(changed.rows[0]["id"], Value::Int(id));
                assert_eq!(changed.rows[0]["_score"], Value::Float(1.0));
                let unchanged = observed
                    .search_knn_with_control(&[1.0, 0.0], 1, &control)
                    .unwrap();
                assert_eq!(
                    unchanged.doc_ids().collect::<Vec<_>>(),
                    found.doc_ids().collect::<Vec<_>>()
                );
                assert_eq!(unchanged.iter().next().unwrap().payload.score, 0.0);
                assert!(!visits.lock().contains(&unseen));
                let outcomes = if writer_first {
                    [second.commit(), first.commit()]
                } else {
                    [first.commit(), second.commit()]
                };
                assert!(outcomes[0].is_ok(), "provider={provider}, observe={observe}, cached={cached}, writer_first={writer_first}: {outcomes:?}");
                if observe {
                    assert_eq!(outcomes[1].as_ref().unwrap_err().sqlstate(), Some("40001"));
                } else {
                    assert!(outcomes[1].is_ok(), "the physical fixture alone must not create the logical dependency: {outcomes:?}");
                }
            }
        }
    }
}
