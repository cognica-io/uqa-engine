//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    build::{DiskANNGenerationOptions, DiskANNMergeOptions, DiskANNPartitionOptions},
    format::PAGE_BYTES,
    pages::DiskANNReadLimits,
    DiskANNCanonicalRead, DiskANNQueryRead, PQTrainingOptions,
};
use crate::vector_index::DiskANNIndexParams;

fn options(dimensions: u32) -> DiskANNMemoryOptions {
    let training = PQTrainingOptions {
        max_samples: 8,
        max_iterations: 2,
        max_centroids: 2,
        seed: 42,
    };
    DiskANNMemoryOptions {
        parameters: DiskANNIndexParams {
            max_degree: 2,
            build_list_size: 4,
            search_list_size: 1,
            beam_width: 1,
            pq_bytes: 1,
            ..DiskANNIndexParams::for_dimensions(dimensions).unwrap()
        },
        read: DiskANNReadLimits {
            resident_bytes: 65_536,
            cache_bytes: 0,
            max_in_flight_page_bytes: 2 * PAGE_BYTES,
            max_record_bytes: 8192,
        },
        partitions: DiskANNPartitionOptions {
            max_partition_points: 8,
            max_depth: 0,
            coarse_training: PQTrainingOptions {
                max_centroids: 3,
                ..training
            },
        },
        merge: DiskANNMergeOptions {
            sort_buffer_records: 8,
        },
        generation: DiskANNGenerationOptions {
            training,
            code_batch_nodes: 4,
            side_batch_entries: 4,
            max_record_bytes: 8192,
        },
    }
}

fn scores(index: &dyn VectorIndex) -> Vec<(DocId, f64)> {
    index
        .search_knn(&[1.0, 0.0], 99)
        .unwrap()
        .iter()
        .map(|posting| (posting.doc_id, posting.payload.score))
        .collect()
}

fn new(control: &StorageReadControl) -> DiskANNMemoryIndex {
    DiskANNMemoryIndex::new(
        2,
        options(2),
        &DiskANNTemporaryBudget::new(1 << 20),
        control,
    )
    .unwrap()
}

#[test]
fn diskann_memory_writes_merge_with_sealed_pages_and_preserve_old_snapshots() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    assert_eq!(index.index_kind(), "diskann");
    assert!(scores(&index).is_empty());
    index
        .add_many(1, vec![vec![0.0, 1.0], vec![1.0, 0.0]])
        .unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    index.add(3, vec![0.0, 0.0]).unwrap();
    index.add(4, vec![-1.0, 0.0]).unwrap();
    let before_build = index.snapshot().unwrap();
    index.initialize().unwrap();
    assert_eq!(index.temporary.used(), 0);
    assert!(index
        .index
        .canonical()
        .next_change_after(None, &control)
        .unwrap()
        .is_none());
    assert_eq!(index.count().unwrap(), 5);
    let expected = [(1, 1.0), (2, 0.0), (3, 0.0), (4, -1.0)];
    assert_eq!(scores(&index), expected);
    assert_eq!(scores(&*before_build), expected);
    let generation = *index.manifest();
    let held = index.snapshot().unwrap();
    index.add(1, vec![-1.0, 0.0]).unwrap();
    index.delete(2).unwrap();
    index.add(5, vec![1.0, 0.0]).unwrap();
    assert_eq!(
        index.manifest(),
        &generation,
        "ordinary writes never rebuild the graph"
    );
    assert_eq!(scores(&index), [(1, -1.0), (3, 0.0), (4, -1.0), (5, 1.0)]);
    assert_eq!(scores(&*held), expected);
    assert_eq!(index.count().unwrap(), 4);
    assert!(!index.contains_document(2).unwrap());
    let threshold = index.search_threshold(&[1.0, 0.0], 0.0).unwrap();
    assert_eq!(threshold.doc_ids().collect::<Vec<_>>(), [3, 5]);
    index.initialize().unwrap();
    assert_ne!(
        index.manifest().input().generation,
        generation.input().generation
    );
    assert_eq!(index.search_threshold(&[1.0, 0.0], 0.0).unwrap(), threshold);
    assert_eq!(scores(&*held), expected);
}

#[test]
fn diskann_memory_writable_forks_keep_independent_roots_and_nonreused_origins_after_undo() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut live = new(&control);
    live.add(1, vec![1.0, 0.0]).unwrap();
    live.initialize().unwrap();
    let savepoint = live.fork().unwrap();
    let mut branch = live.fork().unwrap();
    live.add(1, vec![0.0, 1.0]).unwrap();
    branch.add(1, vec![-1.0, 0.0]).unwrap();
    let version = |index: &DiskANNMemoryIndex| {
        index
            .index
            .canonical()
            .origin(1, &control)
            .unwrap()
            .unwrap()
    };
    let discarded = version(&live);
    let alternate = version(&branch);
    assert_ne!(discarded, alternate);
    assert_eq!(scores(&live), [(1, 0.0)]);
    assert_eq!(scores(&branch), [(1, -1.0)]);
    branch.initialize().unwrap();
    live = savepoint;
    assert_eq!(scores(&live), [(1, 1.0)]);
    live.add(1, vec![1.0, 0.0]).unwrap();
    assert!(version(&live).revision() > alternate.revision());
    assert_ne!(version(&live), discarded);
    assert_ne!(
        live.manifest().input().generation,
        branch.manifest().input().generation
    );
    let mut writable = live.writable_snapshot().unwrap();
    writable.clear().unwrap();
    assert!(scores(&*writable).is_empty());
    assert_eq!(scores(&live), [(1, 1.0)]);
    assert_eq!(scores(&branch), [(1, -1.0)]);
}

#[test]
fn diskann_memory_forks_and_replacements_share_unchanged_tensor_allocations() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut live = DiskANNMemoryIndex::new(
        1024,
        options(1024),
        &DiskANNTemporaryBudget::new(1 << 20),
        &control,
    )
    .unwrap();
    for document in 1..=32 {
        live.add(document, vec![1.0; 1024]).unwrap();
    }
    let pointer = |index: &DiskANNMemoryIndex| {
        let mut pointer = 0;
        index
            .index
            .canonical()
            .visit_document(31, &control, &mut |_, _, raw| {
                pointer = raw.as_ptr() as usize;
                Ok(())
            })
            .unwrap();
        pointer
    };
    let used = control.memory().used();
    let mut branch = live.fork().unwrap();
    assert_eq!(
        control.memory().used() - used,
        size_of::<DiskANNMemoryIndex>()
    );
    assert_eq!(pointer(&live), pointer(&branch));
    branch.add(1, vec![0.0; 1024]).unwrap();
    assert_eq!(pointer(&live), pointer(&branch));
    assert!(
        control.memory().used() - used < 8192,
        "one changed tensor and tree paths, not the 128 KiB corpus"
    );
    drop(branch);
    assert_eq!(control.memory().used(), used);
}

#[test]
fn diskann_memory_failed_mutation_or_rebuild_preserves_both_roots_and_releases_candidates() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.initialize().unwrap();
    let original = *index.manifest();
    let used = control.memory().used();
    let hold = control.memory().reserve((1 << 20) - used).unwrap();
    assert!(index.add(1, vec![-1.0, 0.0]).is_err());
    assert!(index.add_many(2, vec![vec![0.0, 1.0]]).is_err());
    assert!(index.initialize().is_err());
    assert!(index.clear().is_err());
    assert!(index.writable_snapshot().is_err());
    assert_eq!(index.manifest(), &original);
    drop(hold);
    assert_eq!(control.memory().used(), used);
    assert_eq!(scores(&index), [(1, 1.0)]);
    index.add(2, vec![0.0, 1.0]).unwrap();
    let temporary = index.temporary.clone();
    index.temporary = DiskANNTemporaryBudget::new(0);
    let used = control.memory().used();
    assert!(index.initialize().is_err());
    assert_eq!(control.memory().used(), used);
    assert_eq!(index.temporary.used(), 0);
    assert_eq!(index.manifest(), &original);
    assert_eq!(scores(&index), [(1, 1.0), (2, 0.0)]);
    index.temporary = temporary;
    index.initialize().unwrap();
    assert_eq!(scores(&index), [(1, 1.0), (2, 0.0)]);
}

#[test]
fn diskann_memory_clear_publishes_empty_pages_without_invalidating_retained_readers() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.initialize().unwrap();
    let held = index.snapshot().unwrap();
    let generation = index.manifest().input().generation;
    index.clear().unwrap();
    assert_eq!(index.count().unwrap(), 0);
    assert_eq!(index.manifest().input().nodes, 0);
    assert_ne!(index.manifest().input().generation, generation);
    assert!(scores(&index).is_empty());
    index.add(2, vec![0.0, 1.0]).unwrap();
    assert_eq!(scores(&index), [(2, 0.0)]);
    assert_eq!(scores(&*held), [(1, 1.0)]);
    let temporary = index.temporary.clone();
    drop(index);
    assert!(control.memory().used() > 0);
    assert_eq!(scores(&*held), [(1, 1.0)]);
    drop(held);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
}

#[test]
fn diskann_memory_validation_and_exhausted_identity_never_publish_partial_changes() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let used = control.memory().used();
    for vector in [vec![1.0], vec![f32::NAN, 0.0], vec![f32::INFINITY, 0.0]] {
        assert!(index.add(1, vector).is_err());
    }
    assert_eq!(control.memory().used(), used);
    assert_eq!(scores(&index), [(1, 1.0)]);
    index.clock.next.store(u64::MAX, Ordering::Relaxed);
    assert!(index.add(1, vec![-1.0, 0.0]).is_err());
    assert!(index.initialize().is_err());
    assert_eq!(scores(&index), [(1, 1.0)]);
    assert_eq!(control.memory().used(), used);
    let tiny = StorageReadControl::with_limit(1);
    assert!(
        DiskANNMemoryIndex::new(2, options(2), &DiskANNTemporaryBudget::new(0), &tiny).is_err()
    );
    assert_eq!(tiny.memory().used(), 0);
}

#[test]
fn diskann_memory_snapshots_and_writes_keep_original_controls() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let fresh = StorageReadControl::with_limit(1 << 22);
    let nested = index
        .snapshot_with_control(&fresh)
        .unwrap()
        .snapshot_with_control(&fresh)
        .unwrap();
    let used = control.memory().used();
    let hold = control.memory().reserve((1 << 20) - used).unwrap();
    assert!(nested
        .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
        .is_err());
    assert_eq!(fresh.memory().used(), 0);
    drop(hold);
    fresh.cancellation().cancel();
    assert!(index
        .search_knn_with_control(&[1.0, 0.0], 0, &fresh)
        .is_err());
    assert_eq!(scores(&index), [(1, 1.0)]);
    control.cancellation().cancel();
    assert!(index.add(2, vec![1.0, 0.0]).is_err());
    assert!(index.delete(99).is_err());
    assert!(index.clear().is_err());
    assert!(index.initialize().is_err());
    assert!(index.snapshot().is_err());
    assert!(index.writable_snapshot().is_err());
    assert!(nested.count().is_err());
    drop((index, nested));
    assert_eq!(control.memory().used(), 0);
}
