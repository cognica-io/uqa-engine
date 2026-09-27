//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn populations(index: &dyn VectorIndex) -> (u64, u64) {
    let control = StorageReadControl::with_limit(0);
    let statistics = index
        .diskann_query_metadata(&control)
        .unwrap()
        .unwrap()
        .index_statistics(&control)
        .unwrap();
    assert_eq!(control.memory().used(), 0);
    (
        statistics.populations.current_vectors.unwrap(),
        statistics.populations.changed_vectors.unwrap(),
    )
}

#[test]
fn diskann_memory_populations_follow_complete_tensors_generations_and_independent_roots() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    assert_eq!(populations(&index), (0, 0));
    index
        .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    index.add(2, vec![0.0, 0.0]).unwrap();
    assert_eq!(populations(&index), (3, 3));
    index.initialize().unwrap();
    let generation = index.manifest().input().generation;
    let retained = index.snapshot().unwrap();
    assert_eq!(populations(&index), (3, 0));

    // Identical coordinates receive a fresh origin and are no longer covered by the build.
    index
        .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert_eq!(populations(&index), (3, 2));
    index.add(1, vec![1.0, 0.0]).unwrap();
    assert_eq!(populations(&index), (2, 1));
    index.delete(2).unwrap();
    index.add_many(3, vec![]).unwrap();
    assert_eq!(populations(&index), (1, 1));
    assert!(index.add(1, vec![1.0]).is_err());
    assert_eq!(populations(&index), (1, 1));

    let mut branch = index.fork().unwrap();
    branch
        .add_many(4, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert_eq!(populations(&branch), (3, 3));
    assert_eq!(populations(&index), (1, 1));
    assert_eq!(populations(&*retained), (3, 0));
    let before_rebuild = index.snapshot().unwrap();
    index.initialize().unwrap();
    assert_ne!(index.manifest().input().generation, generation);
    assert_eq!(populations(&index), (1, 0));
    assert_eq!(populations(&*before_rebuild), (1, 1));
    assert_eq!(
        index
            .index
            .canonical()
            .population_counts(generation, &control)
            .unwrap(),
        None
    );
    index.clear().unwrap();
    assert_eq!(populations(&index), (0, 0));
    assert_eq!(populations(&*retained), (3, 0));
}

#[test]
fn diskann_calibration_metadata_distinguishes_mutations_generations_and_controls() {
    let owner = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&owner);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let retained = index.snapshot().unwrap();
    let before = retained.diskann_query_metadata(&owner).unwrap().unwrap();
    assert!(before.corpus_fingerprint.is_some());
    index.add(2, vec![0.0, 1.0]).unwrap();
    let changed = index.diskann_query_metadata(&owner).unwrap().unwrap();
    assert_ne!(before.corpus_fingerprint, changed.corpus_fingerprint);
    assert_eq!(
        before.index_fingerprint(&owner).unwrap(),
        changed.index_fingerprint(&owner).unwrap()
    );
    index.initialize().unwrap();
    let rebuilt = index.diskann_query_metadata(&owner).unwrap().unwrap();
    assert_eq!(changed.corpus_fingerprint, rebuilt.corpus_fingerprint);
    assert_ne!(
        changed.index_fingerprint(&owner).unwrap(),
        rebuilt.index_fingerprint(&owner).unwrap()
    );
    assert_eq!(
        retained.diskann_query_metadata(&owner).unwrap().unwrap(),
        before
    );
    let invocation = StorageReadControl::with_limit(1 << 20);
    invocation.cancellation().cancel();
    assert!(retained.diskann_query_metadata(&invocation).is_err());
    owner.cancellation().cancel();
    assert!(retained
        .diskann_query_metadata(&StorageReadControl::with_limit(1 << 20))
        .is_err());
}
