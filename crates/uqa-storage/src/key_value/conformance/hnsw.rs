//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live HNSW handles follow their logical view; retained graph snapshots stay immutable.

use std::sync::Arc;

use crate::key_value::index_keys::{hnsw_metadata_key, hnsw_node_prefix};
use crate::key_value::KeyValueHNSWIndex;
use crate::vector_index::{HNSWIndexParams, VectorIndex};
use crate::{KeyValueStore, KeyValueVectorIndex, StorageBackendResult};

use super::{expect, expect_eq};

const TABLE: &str = "hnsw\0日本語";
const FIELD: &str = "vectors\0日本語";

fn params() -> HNSWIndexParams {
    HNSWIndexParams {
        m: 4,
        ef_construction: 16,
        ef_search: 16,
        rebuild_threshold: 8,
        seed: 7,
    }
}

fn restore(store: Arc<dyn KeyValueStore>, field: &str) -> StorageBackendResult<KeyValueHNSWIndex> {
    KeyValueHNSWIndex::restore(store, TABLE, field, 2, params())
}

fn contents(index: &dyn VectorIndex, expected: &[u64]) -> StorageBackendResult<()> {
    let mut ids = index
        .search_knn(&[1.0, 0.0], 16)?
        .iter()
        .map(|posting| posting.doc_id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    expect_eq(
        &ids.as_slice(),
        &expected,
        "HNSW search matches current visible documents",
    )?;
    expect_eq(
        &index.count()?,
        &expected.len(),
        "HNSW count matches its search view",
    )
}

/// Verify rollback, savepoint branching, definition replacement and canonical-drift validation on a fresh disposable store.
pub fn verify_hnsw_undo(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let mut index = KeyValueHNSWIndex::create(store.clone(), TABLE, FIELD, 2, params())?;
    index.add(1, vec![1.0, 0.0])?;
    index.initialize()?;
    let baseline = index.snapshot()?;
    expect(
        Arc::ptr_eq(&baseline, &index.snapshot()?),
        "unchanged HNSW snapshots reuse the immutable graph",
    )?;

    store.begin_transaction()?;
    index.add(2, vec![0.0, 1.0])?;
    let private = index.snapshot()?;
    store.rollback_transaction()?;
    contents(&index, &[1])?;
    contents(private.as_ref(), &[1, 2])?;
    contents(baseline.as_ref(), &[1])?;

    store.begin_transaction()?;
    store.savepoint("branch")?;
    index.add(2, vec![0.0, 1.0])?;
    let discarded = index.snapshot()?;
    store.rollback_to_savepoint("branch")?;
    let mut other = restore(store.clone(), FIELD)?;
    other.add(3, vec![0.5, 0.5])?;
    contents(&index, &[1, 3])?;
    contents(discarded.as_ref(), &[1, 2])?;
    store.release_savepoint("branch")?;
    store.commit_transaction()?;
    contents(&index, &[1, 3])?;

    let stable = index.snapshot()?;
    expect(
        index.add(6, vec![f32::NAN, 0.0]).is_err(),
        "invalid HNSW mutation rejected",
    )?;
    expect(
        Arc::ptr_eq(&stable, &index.snapshot()?),
        "failed HNSW mutation preserves the cached view",
    )?;
    let restored = restore(store.clone(), FIELD)?;
    store.delete(&hnsw_metadata_key(TABLE, FIELD)?)?;
    expect(
        restored.count().is_err(),
        "warm restored handle rejects missing metadata",
    )?;
    store.delete_prefix(&hnsw_node_prefix(TABLE, FIELD)?)?;
    let mut replacement = KeyValueHNSWIndex::create(store.clone(), TABLE, FIELD, 2, params())?;
    replacement.add(4, vec![0.8, 0.2])?;
    contents(&restored, &[1, 3, 4])?;
    contents(&index, &[1, 3, 4])?;
    contents(stable.as_ref(), &[1, 3])?;

    let mut canonical = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2);
    canonical.add(5, vec![0.2, 0.8])?;
    expect(
        index.count().is_err(),
        "warm cache rejects canonical vector drift",
    )?;
    index.initialize()?;
    contents(&index, &[1, 3, 4, 5])?;
    contents(&restored, &[1, 3, 4, 5])?;

    let changed_params = HNSWIndexParams {
        ef_search: 32,
        ..params()
    };
    let mut candidate = KeyValueHNSWIndex::create(store.clone(), TABLE, FIELD, 2, changed_params)?;
    store.put(b"\0hnsw-unrelated", b"write before creation finishes")?;
    candidate.add(6, vec![0.3, 0.7])?;
    contents(&candidate, &[1, 3, 4, 5, 6])?;
    expect(
        index.count().is_err(),
        "old HNSW handle rejects replaced parameters",
    )?;
    contents(
        &KeyValueHNSWIndex::restore(store, TABLE, FIELD, 2, changed_params)?,
        &[1, 3, 4, 5, 6],
    )
}

/// Verify live and pinned readers while independent HNSW fields commit. Leaves fixtures for `verify_hnsw_reopen`.
pub fn verify_hnsw_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let mut first = KeyValueHNSWIndex::create(a.clone(), TABLE, FIELD, 2, params())?;
    first.add(1, vec![1.0, 0.0])?;
    first.initialize()?;
    let mut observer = restore(b.clone(), FIELD)?;
    a.begin_transaction()?;
    first.add(2, vec![0.0, 1.0])?;
    let private = first.snapshot()?;
    b.begin_transaction()?;
    let mut independent = KeyValueHNSWIndex::create(b.clone(), TABLE, "other", 2, params())?;
    independent.add(11, vec![1.0, 0.0])?;
    independent.initialize()?;
    b.commit_transaction()?;
    expect(
        a.in_transaction(),
        "independent HNSW commit completes while first writer remains private",
    )?;
    contents(&observer, &[1])?;
    contents(&first, &[1, 2])?;
    a.commit_transaction()?;
    contents(&observer, &[1, 2])?;

    a.begin_read_transaction()?;
    observer.add(3, vec![0.8, 0.2])?;
    contents(&first, &[1, 2])?;
    a.commit_transaction()?;
    contents(&first, &[1, 2, 3])?;
    contents(private.as_ref(), &[1, 2])?;

    a.begin_transaction()?;
    a.savepoint("keep")?;
    first.add(4, vec![0.2, 0.8])?;
    let discarded = first.snapshot()?;
    independent.add(12, vec![0.0, 1.0])?;
    a.rollback_to_savepoint("keep")?;
    first.add(5, vec![0.6, 0.4])?;
    a.commit_transaction()?;
    contents(discarded.as_ref(), &[1, 2, 3, 4])?;
    contents(&observer, &[1, 2, 3, 5])?;
    contents(&independent, &[11, 12])
}

/// Verify persisted vectors and graph structure after `verify_hnsw_concurrency` closes every database handle.
pub fn verify_hnsw_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    contents(&restore(store.clone(), FIELD)?, &[1, 2, 3, 5])?;
    contents(&restore(store, "other")?, &[11, 12])
}
