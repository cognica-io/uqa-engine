//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained tensor snapshots remain separate from subsequent session changes.

use std::sync::Arc;

use crate::vector_index::{
    HNSWIndexParams, IVFIndexParams, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};
use crate::{
    KeyValueStorageBackend, KeyValueStore, KeyValueVectorIndex, PersistentStorageBackend,
    StorageBackendError, StorageBackendResult,
};

use super::{expect, expect_eq};

const TABLE: &str = "snapshots\0日本語";

/// Verify exact, IVF and HNSW tensor snapshots through whole/savepoint rollback and after the original handle closes. Requires a disposable store.
pub fn verify_vector_snapshots(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    for spec in [
        VectorIndexSpec::BruteForce,
        VectorIndexSpec::IVF(IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 2,
        }),
        VectorIndexSpec::HNSW(HNSWIndexParams::default()),
    ] {
        verify_vector_snapshot(store, spec)?;
    }
    Ok(())
}

fn verify_vector_snapshot(
    store: &Arc<dyn KeyValueStore>,
    spec: VectorIndexSpec,
) -> StorageBackendResult<()> {
    let field = spec.access_method();
    let mut index = KeyValueStorageBackend::new(store.clone()).vector_index(
        TABLE,
        field,
        2,
        spec,
        VectorIndexOpenMode::Create,
    )?;
    index.add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])?;
    index.initialize()?;
    let baseline = index.snapshot()?;
    store.begin_transaction()?;
    index.add(2, vec![0.5, 0.5])?;
    let private = index.snapshot()?;
    expect(index.contains_document(2)?, "private vector membership")?;
    expect(
        !baseline.contains_document(2)?,
        "retained membership excludes later insertion",
    )?;
    expect_eq(
        &baseline.count()?,
        &2,
        "retained tensor snapshot ignores private insertion",
    )?;
    store.rollback_transaction()?;
    expect(
        !index.contains_document(2)?,
        "live membership observes rollback",
    )?;
    expect(
        private.contains_document(2)?,
        "private membership survives rollback",
    )?;
    expect_eq(&index.count()?, &2, "live tensor index observes rollback")?;
    expect_eq(
        &private.count()?,
        &3,
        "private tensor snapshot survives rollback",
    )?;

    store.begin_transaction()?;
    store.savepoint("vectors")?;
    index.delete(1)?;
    index.add(3, vec![0.25, 0.75])?;
    let discarded = index.snapshot()?;
    expect(
        !discarded.contains_document(1)?,
        "retained membership observes deletion",
    )?;
    store.rollback_to_savepoint("vectors")?;
    index.add(4, vec![0.75, 0.25])?;
    store.commit_transaction()?;
    expect(index.contains_document(1)?, "restored canonical membership")?;
    expect(
        !index.contains_document(3)?,
        "discarded candidate remains absent",
    )?;
    expect(
        discarded.contains_document(3)?,
        "discarded snapshot keeps its candidate",
    )?;
    expect_eq(
        &index.count()?,
        &3,
        "tensor write after savepoint restoration",
    )?;
    expect_eq(
        &discarded.count()?,
        &1,
        "discarded savepoint branch stays retained",
    )?;
    let scored = baseline.search_threshold(&[1.0, 0.0], 0.5)?;
    expect_eq(
        &scored.len(),
        &1,
        "tensor snapshot collapses per-document scores",
    )?;
    expect_eq(
        &scored.iter().next().unwrap().doc_id,
        &1,
        "tensor snapshot retains original document",
    )?;
    drop(index);
    read_only(baseline, 2)?;
    read_only(private, 3)?;
    read_only(discarded, 1)?;
    expect_eq(
        &KeyValueVectorIndex::new(store.clone(), TABLE, field, 2).count()?,
        &3,
        "snapshot mutation cannot modify canonical storage",
    )?;
    Ok(())
}

fn read_only(mut snapshot: Arc<dyn VectorIndex>, count: usize) -> StorageBackendResult<()> {
    let unique = Arc::get_mut(&mut snapshot).ok_or_else(|| {
        StorageBackendError::Other(
            "snapshot unexpectedly still belongs to a live index cache".into(),
        )
    })?;
    expect(
        unique.add(99, vec![1.0, 0.0]).is_err(),
        "retained vector add is rejected",
    )?;
    expect(
        unique.add_many(99, vec![vec![1.0, 0.0]]).is_err(),
        "retained tensor add is rejected",
    )?;
    expect(
        unique.delete(1).is_err(),
        "retained vector delete is rejected",
    )?;
    expect(unique.clear().is_err(), "retained vector clear is rejected")?;
    expect(
        unique.initialize().is_err(),
        "retained vector rebuild is rejected",
    )?;
    expect_eq(
        &unique.count()?,
        &count,
        "rejected snapshot mutations preserve its data",
    )
}

/// Verify exact tensor snapshots and independent document writers over two sessions of a fresh disposable store.
pub fn verify_exact_snapshot_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let mut first = KeyValueVectorIndex::new(a.clone(), TABLE, "exact", 2);
    let mut second = KeyValueVectorIndex::new(b.clone(), TABLE, "exact", 2);
    first.add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])?;
    let committed = first.snapshot()?;
    a.begin_transaction()?;
    first.add(2, vec![0.5, 0.5])?;
    let private = first.snapshot()?;
    second.add(3, vec![0.25, 0.75])?;
    expect(
        a.in_transaction(),
        "independent exact writer completes before first transaction ends",
    )?;
    expect_eq(
        &first.count()?,
        &3,
        "private exact reader keeps its boundary",
    )?;
    expect_eq(
        &committed.count()?,
        &2,
        "committed exact snapshot ignores both writers",
    )?;
    a.commit_transaction()?;
    expect_eq(&first.count()?, &4, "independent exact writes both survive")?;
    expect_eq(
        &private.count()?,
        &3,
        "retained private exact snapshot ignores later commit",
    )?;
    a.begin_read_transaction()?;
    second.add(1, vec![0.5, 0.5])?;
    expect_eq(
        &first.count()?,
        &4,
        "exact reader remains pinned during replacement",
    )?;
    a.commit_transaction()?;
    expect_eq(&first.count()?, &3, "later exact reader sees replacement")?;
    let nested = private.snapshot()?;
    drop(private);
    expect_eq(
        &nested.count()?,
        &3,
        "nested exact snapshot retains the same data",
    )?;
    Ok(())
}
