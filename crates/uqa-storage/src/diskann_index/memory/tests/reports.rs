//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::conformance::verify_diskann_vector_statistics;

#[test]
fn diskann_memory_reports_keep_live_and_retained_generation_work_separate() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    index
        .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    index.add(2, vec![-1.0, 0.0]).unwrap();
    index.initialize().unwrap();
    let generation = index.manifest().input().generation;
    let old = index.snapshot().unwrap();
    verify_diskann_vector_statistics(&index, generation, (2, 3), (0, 0)).unwrap();
    index.add(1, vec![0.0, 1.0]).unwrap();
    verify_diskann_vector_statistics(&index, generation, (1, 1), (1, 1)).unwrap();
    let changed = index.snapshot().unwrap();
    index.initialize().unwrap();
    let rebuilt = index.manifest().input().generation;
    assert_ne!(rebuilt, generation);
    verify_diskann_vector_statistics(&index, rebuilt, (2, 2), (0, 0)).unwrap();
    verify_diskann_vector_statistics(&*old, generation, (2, 3), (0, 0)).unwrap();
    verify_diskann_vector_statistics(&*changed, generation, (1, 1), (1, 1)).unwrap();
    index.clear().unwrap();
    verify_diskann_vector_statistics(&index, index.manifest().input().generation, (0, 0), (0, 0))
        .unwrap();
}
