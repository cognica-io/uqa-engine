//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared deterministic visibility schedules for persistent IVF and physical vector indexes.

use std::sync::Arc;

use crate::key_value::index_keys::{
    hnsw_metadata_key, hnsw_node_prefix, ivf_assignment_prefix, ivf_centroid_prefix,
    ivf_metadata_key,
};
use crate::vector_index::{
    HNSWIndexParams, IVFIndexParams, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};
use crate::{
    KeyValueStorageBackend, KeyValueStore, KeyValueVectorIndex, PersistentStorageBackend,
    StorageBackendResult,
};

use super::{expect, expect_eq};

const TABLE: &str = "physical-vectors\0日本語";
const FIELD: &str = "vectors\0日本語";

#[derive(Clone, Copy)]
enum Kind {
    Graph,
    InvertedFile,
}

impl Kind {
    fn spec(self, changed: bool) -> VectorIndexSpec {
        match self {
            Self::Graph => VectorIndexSpec::HNSW(HNSWIndexParams {
                m: 4,
                ef_construction: 16,
                ef_search: if changed { 32 } else { 16 },
                rebuild_threshold: 8,
                seed: 7,
            }),
            Self::InvertedFile => VectorIndexSpec::IVF(IVFIndexParams {
                nlist: if changed { 3 } else { 2 },
                nprobe: if changed { 3 } else { 2 },
                train_threshold: 2,
            }),
        }
    }
    fn open(
        self,
        store: Arc<dyn KeyValueStore>,
        field: &str,
        mode: VectorIndexOpenMode,
        changed: bool,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        KeyValueStorageBackend::new(store).vector_index(TABLE, field, 2, self.spec(changed), mode)
    }
    fn create(
        self,
        store: Arc<dyn KeyValueStore>,
        field: &str,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.open(store, field, VectorIndexOpenMode::Create, false)
    }
    fn restore(
        self,
        store: Arc<dyn KeyValueStore>,
        field: &str,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.open(store, field, VectorIndexOpenMode::Restore, false)
    }
    fn metadata(self) -> StorageBackendResult<Vec<u8>> {
        match self {
            Self::Graph => hnsw_metadata_key(TABLE, FIELD),
            Self::InvertedFile => ivf_metadata_key(TABLE, FIELD),
        }
    }
    fn remove_structure(self, store: &dyn KeyValueStore) -> StorageBackendResult<()> {
        match self {
            Self::Graph => {
                store.delete_prefix(&hnsw_node_prefix(TABLE, FIELD)?)?;
            }
            Self::InvertedFile => {
                store.delete_prefix(&ivf_centroid_prefix(TABLE, FIELD)?)?;
                store.delete_prefix(&ivf_assignment_prefix(TABLE, FIELD)?)?;
            }
        }
        Ok(())
    }
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
        "physical vector search matches current visible documents",
    )?;
    expect_eq(
        &index.count()?,
        &expected.len(),
        "physical vector count matches its search view",
    )
}

/// Verify rollback, savepoint branching, definition replacement and canonical-drift validation on a fresh disposable store.
fn verify_undo(store: Arc<dyn KeyValueStore>, kind: Kind) -> StorageBackendResult<()> {
    let mut index = kind.create(store.clone(), FIELD)?;
    index.add(1, vec![1.0, 0.0])?;
    index.initialize()?;
    let baseline = index.snapshot()?;
    expect(
        Arc::ptr_eq(&baseline, &index.snapshot()?),
        "unchanged physical vector snapshots reuse the immutable graph",
    )?;

    store.begin_transaction()?;
    index.add(2, vec![0.0, 1.0])?;
    let private = index.snapshot()?;
    store.rollback_transaction()?;
    contents(index.as_ref(), &[1])?;
    contents(private.as_ref(), &[1, 2])?;
    contents(baseline.as_ref(), &[1])?;

    store.begin_transaction()?;
    store.savepoint("branch")?;
    index.add(2, vec![0.0, 1.0])?;
    let discarded = index.snapshot()?;
    store.rollback_to_savepoint("branch")?;
    let mut other = kind.restore(store.clone(), FIELD)?;
    other.add(3, vec![0.5, 0.5])?;
    contents(index.as_ref(), &[1, 3])?;
    contents(discarded.as_ref(), &[1, 2])?;
    store.release_savepoint("branch")?;
    store.commit_transaction()?;
    contents(index.as_ref(), &[1, 3])?;

    let stable = index.snapshot()?;
    expect(
        index.add(6, vec![f32::NAN, 0.0]).is_err(),
        "invalid physical vector mutation rejected",
    )?;
    expect(
        Arc::ptr_eq(&stable, &index.snapshot()?),
        "failed physical vector mutation preserves the cached view",
    )?;
    let restored = kind.restore(store.clone(), FIELD)?;
    store.delete(&kind.metadata()?)?;
    expect(
        restored.count().is_err(),
        "warm restored handle rejects missing metadata",
    )?;
    kind.remove_structure(store.as_ref())?;
    let mut replacement = kind.create(store.clone(), FIELD)?;
    replacement.add(4, vec![0.8, 0.2])?;
    contents(restored.as_ref(), &[1, 3, 4])?;
    contents(index.as_ref(), &[1, 3, 4])?;
    contents(stable.as_ref(), &[1, 3])?;

    let mut canonical = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2);
    canonical.add(5, vec![0.2, 0.8])?;
    expect(
        index.count().is_err(),
        "warm cache rejects canonical vector drift",
    )?;
    index.initialize()?;
    contents(index.as_ref(), &[1, 3, 4, 5])?;
    contents(restored.as_ref(), &[1, 3, 4, 5])?;

    let mut candidate = kind.open(store.clone(), FIELD, VectorIndexOpenMode::Create, true)?;
    store.put(b"\0vector-unrelated", b"write before creation finishes")?;
    candidate.add(6, vec![0.3, 0.7])?;
    contents(candidate.as_ref(), &[1, 3, 4, 5, 6])?;
    expect(
        index.count().is_err(),
        "old physical vector handle rejects replaced parameters",
    )?;
    contents(
        kind.open(store, FIELD, VectorIndexOpenMode::Restore, true)?
            .as_ref(),
        &[1, 3, 4, 5, 6],
    )
}

/// Verify live and pinned readers while independent physical vector fields commit. Leaves fixtures for `verify_hnsw_reopen`.
fn verify_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
    kind: Kind,
) -> StorageBackendResult<()> {
    let mut first = kind.create(a.clone(), FIELD)?;
    first.add(1, vec![1.0, 0.0])?;
    first.initialize()?;
    let mut observer = kind.restore(b.clone(), FIELD)?;
    a.begin_transaction()?;
    first.add(2, vec![0.0, 1.0])?;
    let private = first.snapshot()?;
    b.begin_transaction()?;
    let mut independent = kind.create(b.clone(), "other")?;
    independent.add(11, vec![1.0, 0.0])?;
    independent.initialize()?;
    b.commit_transaction()?;
    expect(
        a.in_transaction(),
        "independent physical vector commit completes while first writer remains private",
    )?;
    contents(observer.as_ref(), &[1])?;
    contents(first.as_ref(), &[1, 2])?;
    a.commit_transaction()?;
    contents(observer.as_ref(), &[1, 2])?;

    a.begin_read_transaction()?;
    observer.add(3, vec![0.8, 0.2])?;
    contents(first.as_ref(), &[1, 2])?;
    a.commit_transaction()?;
    contents(first.as_ref(), &[1, 2, 3])?;
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
    contents(observer.as_ref(), &[1, 2, 3, 5])?;
    contents(independent.as_ref(), &[11, 12])
}

/// Verify persisted vectors and graph structure after `verify_hnsw_concurrency` closes every database handle.
fn verify_reopen(store: Arc<dyn KeyValueStore>, kind: Kind) -> StorageBackendResult<()> {
    contents(kind.restore(store.clone(), FIELD)?.as_ref(), &[1, 2, 3, 5])?;
    contents(kind.restore(store, "other")?.as_ref(), &[11, 12])
}

/// Verify HNSW rollback, definition recreation and canonical-vector drift on a disposable store.
pub fn verify_hnsw_undo(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    verify_undo(store, Kind::Graph)
}
/// Verify HNSW visibility between two independent sessions; leaves fixtures for `verify_hnsw_reopen`.
pub fn verify_hnsw_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    verify_concurrency(a, b, Kind::Graph)
}
/// Verify the durable HNSW fixture after all prior handles close.
pub fn verify_hnsw_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    verify_reopen(store, Kind::Graph)
}
/// Verify IVF rollback, definition recreation and canonical-vector cardinality validation on a disposable store.
pub fn verify_ivf_undo(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    verify_undo(store, Kind::InvertedFile)
}
/// Verify IVF visibility between two independent sessions; leaves fixtures for `verify_ivf_reopen`.
pub fn verify_ivf_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    verify_concurrency(a, b, Kind::InvertedFile)
}
/// Verify the durable IVF fixture after all prior handles close.
pub fn verify_ivf_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    verify_reopen(store, Kind::InvertedFile)
}
