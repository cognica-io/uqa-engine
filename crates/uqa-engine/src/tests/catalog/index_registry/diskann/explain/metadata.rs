//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{DocId, PostingList};
use uqa_storage::{
    diskann_index::DiskANNQueryMetadata, read_control::StorageReadControl, StorageBackendResult,
    VectorIndex,
};

struct MetadataOnly {
    inner: Box<dyn VectorIndex>,
    metadata_reads: Arc<AtomicUsize>,
}

impl VectorIndex for MetadataOnly {
    fn dimensions(&self) -> u32 {
        self.inner.dimensions()
    }
    fn index_kind(&self) -> &'static str {
        self.inner.index_kind()
    }
    fn diskann_query_metadata(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNQueryMetadata>> {
        self.metadata_reads.fetch_add(1, Ordering::Relaxed);
        self.inner.diskann_query_metadata(control)
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        panic!("EXPLAIN must not write")
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        panic!("EXPLAIN must not write")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("EXPLAIN must not write")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("EXPLAIN must not clear an index")
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        panic!("EXPLAIN must not build an index")
    }
    fn search_knn(&self, _: &[f32], _: usize) -> StorageBackendResult<PostingList> {
        panic!("EXPLAIN must not search")
    }
    fn search_threshold(&self, _: &[f32], _: f32) -> StorageBackendResult<PostingList> {
        panic!("EXPLAIN must not search")
    }
    fn count(&self) -> StorageBackendResult<usize> {
        panic!("EXPLAIN must not count the canonical corpus")
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        panic!("EXPLAIN must not prepare an executable snapshot")
    }
    fn diskann_read_snapshot(
        &self,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<uqa_storage::diskann_index::DiskANNReadSnapshot>> {
        panic!("EXPLAIN must not prepare canonical readers")
    }
}

fn check(engine: &Engine) {
    let snapshot = engine.capture_statement_read_snapshot().unwrap();
    let reader = engine.statement_read_snapshot_engine(&snapshot);
    let reads = Arc::new(AtomicUsize::new(0));
    let table = reader.try_query_table("diskann_docs").unwrap().unwrap();
    {
        let mut indexes = table.vector_indexes.write();
        let inner = indexes.get("embedding").unwrap().snapshot().unwrap();
        let mut registrations = std::collections::BTreeMap::new();
        registrations.insert(
            "embedding".into(),
            Box::new(MetadataOnly {
                inner: Box::new(uqa_storage::ReadOnlySnapshot::new(inner)),
                metadata_reads: reads.clone(),
            }) as Box<dyn VectorIndex>,
        );
        // Wrap the selected provider view; the live registry is refreshed at SQL entry.
        *indexes = registrations.into();
    }
    let plan = explain(
        &reader,
        "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&plan).len(), 1);
    assert!(reads.load(Ordering::Relaxed) > 0);
    reads.store(0, Ordering::Relaxed);
    let invalid = explain(
        &reader,
        "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0],1)",
    );
    assert!(nodes(&invalid).is_empty());
    assert_eq!(reads.load(Ordering::Relaxed), 0);
}

#[test]
fn diskann_explain_uses_only_selected_metadata_capabilities_after_provider_reopen() {
    let memory = Engine::new();
    fixture(&memory);
    check(&memory);
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        fixture(&engine);
        let factory = engine.storage.provider.as_ref().unwrap().clone();
        drop((engine, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        check(&reopened);
    }
}
