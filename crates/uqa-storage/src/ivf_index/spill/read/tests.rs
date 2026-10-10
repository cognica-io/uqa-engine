//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{KeyValueIVFIndex, MemoryKeyValueStore},
    IVFIndex, IVFIndexParams,
};

#[test]
fn write_only_ivf_generations_do_not_rebuild_ranked_posting_lists() {
    let mut index = KeyValueIVFIndex::create(
        Arc::new(MemoryKeyValueStore::new()),
        "items",
        "vector",
        2,
        IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 8,
        },
    )
    .unwrap();
    let mut reference = IVFIndex::with_params(2, 2, 2, 8);
    LIST_BUILDS.set(0);
    for document in 1..=32 {
        let vector = vec![document as f32, 1.0];
        index.add(document, vector.clone()).unwrap();
        reference.add(document, vector).unwrap();
    }
    assert_eq!(LIST_BUILDS.get(), 0);
    let expected = reference.search_knn(&[1.0, 1.0], 8).unwrap();
    for _ in 0..2 {
        assert_eq!(index.search_knn(&[1.0, 1.0], 8).unwrap(), expected);
    }
    assert_eq!(LIST_BUILDS.get(), 1);
    let retained = index.snapshot().unwrap();
    index.add(33, vec![1.0, 0.0]).unwrap();
    assert_eq!(LIST_BUILDS.get(), 1);
    assert_eq!(retained.search_knn(&[1.0, 1.0], 8).unwrap(), expected);
    assert_eq!(LIST_BUILDS.get(), 1);
}
