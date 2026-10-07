//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful mutations retain the evaluated graph under its actual visibility identity.

use super::*;
use crate::key_value::MemoryKeyValueStore;

#[test]
fn sequential_hnsw_mutations_do_not_restore_the_entire_graph() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index =
        KeyValueHNSWIndex::create(store, "items", "vector", 2, HNSWIndexParams::default()).unwrap();
    let mut expected = HNSWIndex::new(2);
    hnsw_persistence::RESTORED_GRAPHS.set(0);
    for id in 1..=32 {
        index.add(id, vec![id as f32, 1.0]).unwrap();
        expected.add(id, vec![id as f32, 1.0]).unwrap();
        assert_eq!(
            index.read_graph().unwrap().value.persistence_snapshot(),
            expected.persistence_snapshot()
        );
    }
    index.add(7, vec![-1.0, 0.0]).unwrap();
    expected.add(7, vec![-1.0, 0.0]).unwrap();
    index.delete(19).unwrap();
    expected.delete(19).unwrap();
    assert_eq!(
        index.read_graph().unwrap().value.persistence_snapshot(),
        expected.persistence_snapshot()
    );
    assert_eq!(
        index.search_knn(&[-1.0, 0.0], 8).unwrap(),
        expected.search_knn(&[-1.0, 0.0], 8).unwrap()
    );
    assert_eq!(index.count().unwrap(), 31);
    assert_eq!(
        hnsw_persistence::RESTORED_GRAPHS.get(),
        0,
        "own mutations must retain their evaluated graph"
    );
    index.clear().unwrap();
    assert_eq!(index.read_graph().unwrap().value.count().unwrap(), 0);
    assert_eq!(hnsw_persistence::RESTORED_GRAPHS.get(), 0);
}

#[test]
fn mutation_revisions_preserve_memory_undo_and_failed_evaluation() {
    crate::key_value::conformance::verify_mutation_revisions(&MemoryKeyValueStore::new()).unwrap();
}

#[test]
fn uncertified_mutations_restore_from_the_actual_read_boundary() {
    let store = Arc::new(Uncertified(MemoryKeyValueStore::new()));
    let mut index =
        KeyValueHNSWIndex::create(store, "items", "vector", 2, HNSWIndexParams::default()).unwrap();
    hnsw_persistence::RESTORED_GRAPHS.set(0);
    for id in 1..=32 {
        index.add(id, vec![id as f32, 1.0]).unwrap();
    }
    assert_eq!(index.count().unwrap(), 32);
    assert_eq!(index.read_graph().unwrap().value.count().unwrap(), 32);
    assert_eq!(hnsw_persistence::RESTORED_GRAPHS.get(), 32);
}

struct Uncertified(MemoryKeyValueStore);

impl KeyValueStore for Uncertified {
    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        self.0.get(key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.0.put(key, value)
    }
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.0.delete(key)
    }
    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.0.scan_prefix(prefix)
    }
    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        self.0.delete_prefix(prefix)
    }
    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        self.0.batch()
    }
    fn in_transaction(&self) -> bool {
        self.0.in_transaction()
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.0.transaction_has_written()
    }
    fn with_read_view(
        &self,
        read: &mut crate::key_value::KeyValueReadScope<'_>,
    ) -> StorageBackendResult<()> {
        self.0.with_read_view(read)
    }
    fn with_mutation(
        &self,
        mutate: &mut crate::key_value::KeyValueMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.0.with_mutation(mutate)
    }
}
