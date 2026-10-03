//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable HNSW publication and retained readers with a corpus larger than the session allowance.

use std::sync::Arc;

use super::{expect, expect_eq};
use crate::{
    hnsw_index::HNSWIndex,
    key_value::KeyValueHNSWIndex,
    vector_index::{HNSWIndexParams, VectorIndex},
    KeyValueStore, KeyValueVectorIndex, StorageBackendError, StorageBackendResult,
};

const TABLE: &str = "bounded_hnsw";
const FIELD: &str = "embedding";
const DIMENSIONS: u32 = 4096;
const DOCUMENTS: u64 = 20;

fn params() -> HNSWIndexParams {
    HNSWIndexParams {
        m: 2,
        ef_construction: 4,
        ef_search: 24,
        rebuild_threshold: 3,
        ..HNSWIndexParams::default()
    }
}

fn vector(document: u64) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS as usize];
    vector[document as usize % DIMENSIONS as usize] = 1.0;
    vector[0] = 0.125;
    vector
}

fn reference() -> StorageBackendResult<HNSWIndex> {
    let mut index = HNSWIndex::with_params(DIMENSIONS, params())?;
    for document in 1..=DOCUMENTS {
        index.add(document, vector(document))?;
    }
    Ok(index)
}

/// Run on a fresh disposable store configured with a 256 KiB retained allowance.
pub fn verify_hnsw_spill_lifecycle(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = store.retention_control().expect("bounded logical session");
    expect_eq(
        &control.memory().limit(),
        &(256 * 1024),
        "fixture retention allowance",
    )?;
    expect(
        DOCUMENTS as usize * DIMENSIONS as usize * size_of::<f32>() > control.memory().limit(),
        "canonical coordinates exceed retention allowance",
    )?;
    let mut raw = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, DIMENSIONS);
    for document in 1..=DOCUMENTS {
        raw.add(document, vector(document))?;
    }
    drop(raw);
    let mut index = KeyValueHNSWIndex::create(store.clone(), TABLE, FIELD, DIMENSIONS, params())
        .map_err(|error| StorageBackendError::backend("bounded HNSW construction", error))?;
    index
        .initialize()
        .map_err(|error| StorageBackendError::backend("bounded HNSW publication", error))?;
    let mut reference = reference()?;
    let query = vector(17);
    let expected = reference.search_knn(&query, 5)?;
    expect_eq(
        &index.search_knn(&query, 5)?,
        &expected,
        "bounded HNSW build scores",
    )?;
    let retained = index.snapshot()?;

    store.begin_transaction()?;
    store.savepoint("before")?;
    index.add(90, query.clone())?;
    let discarded = index.snapshot()?;
    store.rollback_to_savepoint("before")?;
    store.release_savepoint("before")?;
    expect_eq(
        &index.search_knn(&query, 5)?,
        &expected,
        "spilled savepoint rollback",
    )?;
    expect_eq(
        &discarded.count()?,
        &(DOCUMENTS as usize + 1),
        "retained discarded graph",
    )?;
    drop(discarded);
    index.delete(1)?;
    store.rollback_transaction()?;
    expect_eq(
        &index.count()?,
        &(DOCUMENTS as usize),
        "spilled transaction rollback",
    )?;

    index.add(17, vector(250))?;
    index.delete(8)?;
    index.delete(9)?;
    reference.add(17, vector(250))?;
    reference.delete(8)?;
    reference.delete(9)?;
    expect_eq(
        &index.search_knn(&query, 5)?,
        &reference.search_knn(&query, 5)?,
        "spilled mutation and compaction",
    )?;
    expect_eq(
        &retained.search_knn(&query, 5)?,
        &expected,
        "retained original graph",
    )?;
    expect(
        control.memory().peak() <= control.memory().limit(),
        "original allowance enforced",
    )?;
    drop((index, store));
    expect_eq(
        &retained.search_knn(&query, 5)?,
        &expected,
        "reader survives mutable handle and session",
    )?;
    drop(retained);
    expect_eq(
        &control.memory().used(),
        &0,
        "final graph allowance released",
    )?;
    Ok(())
}

/// Reopen the disposable store after `verify_hnsw_spill_lifecycle` with the same allowance.
pub fn verify_hnsw_spill_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let index = KeyValueHNSWIndex::restore(store, TABLE, FIELD, DIMENSIONS, params())?;
    let mut reference = reference()?;
    reference.add(17, vector(250))?;
    reference.delete(8)?;
    reference.delete(9)?;
    expect_eq(
        &index.count()?,
        &(DOCUMENTS as usize - 2),
        "reopened spilled graph count",
    )?;
    expect_eq(
        &index.search_knn(&vector(17), 5)?,
        &reference.search_knn(&vector(17), 5)?,
        "reopened spilled graph scores",
    )
}
