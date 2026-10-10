//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{key_value::MemoryKeyValueStore, IVFIndex};

fn params() -> IVFIndexParams {
    IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 8,
    }
}

#[test]
fn sequential_ivf_mutations_retain_the_certified_generation() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index = KeyValueIVFIndex::create(store, "items", "vector", 2, params()).unwrap();
    let mut reference = IVFIndex::with_params(2, 2, 2, 8);
    ivf_persistence::RESTORED_INDEXES.set(0);
    for document in 1..=32 {
        let vector = vec![document as f32, 1.0];
        index.add(document, vector.clone()).unwrap();
        reference.add(document, vector).unwrap();
        assert_eq!(
            index.read_index().unwrap().value.count().unwrap(),
            document as usize
        );
        assert_eq!(
            index.search_knn(&[1.0, 1.0], 8).unwrap(),
            reference.search_knn(&[1.0, 1.0], 8).unwrap()
        );
    }
    let retained = index.snapshot().unwrap();
    index
        .add_many(7, vec![vec![-1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    reference
        .add_many(7, vec![vec![-1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    index.delete(19).unwrap();
    reference.delete(19).unwrap();
    assert_eq!(
        index.search_knn(&[-1.0, 0.0], 8).unwrap(),
        reference.search_knn(&[-1.0, 0.0], 8).unwrap()
    );
    assert!(retained.contains_document(19).unwrap());
    assert!(!index.snapshot().unwrap().contains_document(19).unwrap());
    index.clear().unwrap();
    assert_eq!(index.read_index().unwrap().value.count().unwrap(), 0);
    assert_eq!(retained.count().unwrap(), 32);
    assert_eq!(ivf_persistence::RESTORED_INDEXES.get(), 0);
}

#[test]
fn ivf_candidates_preserve_undo_and_failed_staging() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index =
        KeyValueIVFIndex::create(store.clone(), "items", "vector", 2, params()).unwrap();
    index.add(1, vec![1.0, 0.0]).unwrap();
    store.begin_transaction().unwrap();
    store.savepoint("before").unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    let retained = index.snapshot().unwrap();
    ivf_persistence::RESTORED_INDEXES.set(0);
    assert!(index
        .mutate(IVFMutation::Delete(1), |_| Err(other_error(
            "injected staging failure"
        )))
        .is_err());
    assert_eq!(index.read_index().unwrap().value.count().unwrap(), 2);
    assert_eq!(ivf_persistence::RESTORED_INDEXES.get(), 0);
    store.rollback_to_savepoint("before").unwrap();
    assert_eq!(index.read_index().unwrap().value.count().unwrap(), 1);
    assert_eq!(ivf_persistence::RESTORED_INDEXES.get(), 1);
    index.add(3, vec![-1.0, 0.0]).unwrap();
    assert!(!index.snapshot().unwrap().contains_document(2).unwrap());
    assert!(retained.contains_document(2).unwrap());
    store.rollback_transaction().unwrap();
    assert_eq!(index.read_index().unwrap().value.count().unwrap(), 1);
    assert_eq!(ivf_persistence::RESTORED_INDEXES.get(), 2);
}
