//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual publication identities, undo branches and bounded canonical views on disposable providers.

mod binding;
pub use binding::verify_diskann_runtime_adoption_conflicts;
pub use binding::{
    diskann_runtime_fixture_options, verify_diskann_runtime_lifecycle,
    verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_reclamation,
    verify_diskann_runtime_reopen, verify_diskann_runtime_retirement,
    verify_diskann_runtime_retirement_reopen,
};
pub use binding::{verify_diskann_live_reopen, verify_diskann_live_writes};
pub use binding::{verify_diskann_pruning, verify_diskann_pruning_reopen};
pub use binding::{
    verify_diskann_publication, verify_diskann_publication_ownership,
    verify_diskann_publication_reopen,
};
pub use binding::{verify_diskann_query_reopen, verify_diskann_query_views};
mod catalog;
pub use binding::verify_diskann_catalog_binding;
pub use binding::{verify_diskann_catalog_identity, verify_diskann_catalog_identity_reopen};
mod changes;
mod corpus;
mod coverage;
mod lifecycle;
pub use lifecycle::verify_mutation_origins;

use super::{expect, expect_eq};
use crate::diskann_index::{
    format::{DiskANNChangeIdentity, DiskANNVectorVersion},
    DiskANNCanonicalRead, DiskANNCanonicalScorer,
};
use crate::key_value::vector_index::origin::journal;
use crate::key_value::{KeyValueDiskANNCanonical, KeyValueVectorIndex, RetainedDiskANNCanonical};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult, VectorIndex};
use std::sync::Arc;

fn canonical(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<KeyValueDiskANNCanonical> {
    KeyValueDiskANNCanonical::new(store.clone(), "diskann-origins", "embedding", 2)
}

fn values(
    source: &RetainedDiskANNCanonical,
    document: u64,
    expected: &[Vec<f32>],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut ordinal = 0;
    let origin = source.origin(document, control)?;
    let visited = source.visit_document(document, control, &mut |index, version, value| {
        expect_eq(
            &Some(version),
            &origin,
            "one origin for all canonical ordinals",
        )?;
        expect_eq(&(index as usize), &ordinal, "contiguous canonical ordinals")?;
        let expected = expected.get(ordinal).ok_or_else(|| {
            crate::StorageBackendError::Other("unexpected canonical vector".into())
        })?;
        expect(
            value
                .iter()
                .map(|value| value.to_bits())
                .eq(expected.iter().map(|value| value.to_bits())),
            "canonical coordinate bits",
        )?;
        ordinal += 1;
        Ok(())
    })?;
    expect_eq(&visited, &origin, "same origin after streaming")?;
    expect_eq(&ordinal, &expected.len(), "complete canonical tensor")
}

/// Exercise actual canonical keys and generated origins, not caller-supplied synthetic versions. Returns the final durable document-one origin for cold reopen.
pub fn verify_diskann_canonical_origins(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNVectorVersion> {
    let control = StorageReadControl::with_limit(1 << 20);
    let index = canonical(store)?;
    let tensor = vec![vec![-0.0, 1.0], vec![f32::MAX, 0.0]];
    let original = index.replace(1, &tensor, &control)?;
    index.replace(2, &[], &control)?;
    let retained = index.retain(&control)?;
    expect_eq(
        &retained.origin(1, &control)?,
        &Some(original),
        "actual original writer persisted",
    )?;
    values(&retained, 1, &tensor, &control)?;
    values(&retained, 2, &[], &control)?;
    expect(
        retained.origin(99, &control)?.is_none(),
        "missing canonical document",
    )?;
    store.begin_transaction()?;
    store.savepoint("origin")?;
    let undone = index.replace(1, &[vec![3.0, 4.0]], &control)?;
    let private = index.retain(&control)?;
    store.rollback_to_savepoint("origin")?;
    let next = index.replace(1, &[vec![5.0, 6.0]], &control)?;
    expect(
        next.writer() == undone.writer() && next.revision() > undone.revision(),
        "savepoint undo cannot reuse an origin",
    )?;
    store.release_savepoint("origin")?;
    store.commit_transaction()?;
    values(&private, 1, &[vec![3.0, 4.0]], &control)?;
    values(&retained, 1, &tensor, &control)?;
    let fresh = index.retain(&control)?;
    values(&fresh, 1, &[vec![5.0, 6.0]], &control)?;
    let mut legacy = KeyValueVectorIndex::new(store.clone(), "diskann-origins", "embedding", 2);
    legacy.add(1, vec![7.0, 8.0])?;
    expect(
        index.retain(&control)?.origin(1, &control).is_err(),
        "unstamped legacy replacement fails closed",
    )?;
    legacy.delete(1)?;
    expect(
        index.retain(&control)?.origin(1, &control)?.is_none(),
        "ordinary delete invalidates origins",
    )?;
    let final_origin = index.replace(1, &[vec![9.0, -0.0]], &control)?;
    concurrent(store, &control)?;
    bounded(store, &control)?;
    catalog::verify(store, &control)?;
    corpus::verify(store)?;
    changes::verify(store)?;
    coverage::verify(store)?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        fresh
            .visit_document(1, &tiny, &mut |_, _, _| Ok(()))
            .is_err(),
        "bounded canonical point reads reject exhausted allowance",
    )?;
    expect_eq(
        &tiny.memory().used(),
        &0,
        "failed canonical workspace released",
    )?;
    let cancelled = StorageReadControl::with_limit(8192);
    cancelled.cancellation().cancel();
    expect(
        fresh.origin(1, &cancelled).is_err(),
        "current query cancellation retained",
    )?;
    drop(index);
    values(&fresh, 1, &[vec![5.0, 6.0]], &control)?;
    Ok(final_origin)
}

/// A fresh provider owner must recover the origin and tensor together.
pub fn verify_diskann_canonical_reopen(
    store: &Arc<dyn KeyValueStore>,
    expected: DiskANNVectorVersion,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let source = canonical(store)?.retain(&control)?;
    expect_eq(
        &source.origin(1, &control)?,
        &Some(expected),
        "canonical origin survives cold reopen",
    )?;
    values(&source, 1, &[vec![9.0, -0.0]], &control)?;
    corpus::verify_reopen(store)?;
    changes::verify_reopen(store)
}

fn concurrent(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for reverse in [false, true] {
        let a = store.open_session()?;
        let b = store.open_session()?;
        a.begin_transaction()?;
        b.begin_transaction()?;
        let offset = u64::from(reverse) * 10;
        let va = canonical(&a)?.replace(100 + offset, &[vec![1.0, 0.0]], control)?;
        let vb = canonical(&b)?.replace(101 + offset, &[vec![0.0, 1.0]], control)?;
        expect(
            va.writer() != vb.writer(),
            "independent origins have independent writers",
        )?;
        if reverse {
            b.commit_transaction()?;
            a.commit_transaction()?;
        } else {
            a.commit_transaction()?;
            b.commit_transaction()?;
        }
        let source = canonical(store)?.retain(control)?;
        expect_eq(
            &source.origin(100 + offset, control)?,
            &Some(va),
            "first disjoint replacement survives",
        )?;
        expect_eq(
            &source.origin(101 + offset, control)?,
            &Some(vb),
            "second disjoint replacement survives",
        )?;
    }
    let a = store.open_session()?;
    let b = store.open_session()?;
    a.begin_transaction()?;
    b.begin_transaction()?;
    let committed = canonical(&a)?.replace(200, &[], control)?;
    let discarded = canonical(&b)?.replace(200, &[vec![1.0, 1.0]], control)?;
    a.commit_transaction()?;
    expect(
        b.commit_transaction().is_err(),
        "empty tensor origin guards conflicting writer",
    )?;
    b.rollback_transaction()?;
    let source = canonical(store)?.retain(control)?;
    expect_eq(
        &source.next_change_after(Some(199), control)?,
        &Some(DiskANNChangeIdentity::new(200, committed)),
        "conflicting writer cannot replace the committed change",
    )?;
    expect(
        store
            .get(&journal::key(
                "diskann-origins",
                "embedding",
                DiskANNChangeIdentity::new(200, discarded),
            )?)?
            .is_none(),
        "conflicting publication leaves no orphan change",
    )?;
    values(&canonical(store)?.retain(control)?, 200, &[], control)
}

fn bounded(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let empty =
        KeyValueDiskANNCanonical::new(store.clone(), "diskann-origins", "empty-wide", 16_384)?;
    empty.replace(1, &[], control)?;
    let empty_source = empty.retain(control)?;
    values(&empty_source, 1, &[], &StorageReadControl::with_limit(8192))?;
    let index = KeyValueDiskANNCanonical::new(store.clone(), "diskann-origins", "wide", 32)?;
    let vectors = vec![vec![1.0; 32]; 128];
    index.replace(1, &vectors, control)?;
    let source = index.retain(control)?;
    let query = StorageReadControl::with_limit(8192);
    values(&source, 1, &vectors, &query)?;
    let mut count = 0;
    source.visit_all(&query, &mut |doc, ordinal, _, raw| {
        expect_eq(
            &(doc, ordinal),
            &(1, count),
            "bounded corpus ordinal identity",
        )?;
        expect(raw == [1.0; 32], "bounded corpus raw coordinates")?;
        count += 1;
        Ok(())
    })?;
    expect_eq(
        &count,
        &128,
        "complete tensor streams through corpus boundary",
    )?;
    let scorer = DiskANNCanonicalScorer::new(&source, &[1.0; 32], &query)?;
    let document = scorer.score_document(1)?.expect("wide fixture tensor");
    expect_eq(
        &document.vector_count(),
        &128,
        "score consumes every bounded tensor ordinal",
    )?;
    expect_eq(
        &scorer.search_exact_knn(1)?.doc_ids().collect::<Vec<_>>(),
        &vec![1],
        "bounded tensor exact query",
    )?;
    expect_eq(
        &query.memory().used(),
        &0,
        "streamed canonical buffers released",
    )?;
    expect(
        query.memory().peak() < 128 * 32 * 4,
        "tensor larger than query allowance streams without materialization",
    )
}
