//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::AtomicBool;
use uqa_core::io_usage::{IOContext, IOObservation, IOOptions};
use uqa_core::memory::MemoryBudget;

fn temporary_reads(operation: impl FnOnce()) -> u64 {
    let observation = IOObservation::new(
        &MemoryBudget::new(16 * 1024),
        IOOptions {
            buffers: true,
            wal: false,
        },
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .unwrap();
    let _scope = IOContext::new(Some(observation.clone())).enter();
    operation();
    observation.snapshot().unwrap().temporary.reads
}

fn probe_work(count: usize) -> (u64, u64) {
    let mut set = crate::distinct::SeenKeySet::new(0, None);
    let mut join = HybridHashIndex::new(0);
    let mut keys = Vec::new();
    for n in 0..count {
        // All bytes share their low six bits across keys. The old 64-way spill put every record in one file.
        let mut key = vec![0; 2048];
        for (digit, byte) in key.iter_mut().take(8).enumerate() {
            *byte = (((n >> (digit * 2)) & 3) << 6) as u8;
        }
        assert!(set.insert(key.clone()).unwrap());
        join.insert(EncodedKey::from_slice(&key), n as u64).unwrap();
        keys.push(key);
    }
    let distinct_reads = temporary_reads(|| {
        for key in keys.iter().rev() {
            assert!(set.contains(key).unwrap());
        }
    });
    let join_reads = temporary_reads(|| {
        for (index, key) in keys.iter().enumerate().rev() {
            let mut rows = join.match_cursor(EncodedKey::from_slice(key)).unwrap();
            assert_eq!(rows.next().unwrap(), Some(index as u64));
            assert_eq!(rows.next().unwrap(), None);
        }
    });
    (distinct_reads, join_reads)
}

#[test]
fn spilled_distinct_and_join_bound_physical_reads_for_colliding_partitions() {
    let small = probe_work(32);
    let large = probe_work(128);
    eprintln!(
        "Temporary block reads for (DISTINCT, HashJoin): 32 keys {small:?}, 128 keys {large:?}"
    );
    assert!(
        small.0 > 0 && small.1 > 0 && large.0 <= small.0 * 7 && large.1 <= small.1 * 7,
        "temporary block reads for (DISTINCT, HashJoin) with 32 keys: {small:?}; with 128 keys: {large:?}"
    );
}
