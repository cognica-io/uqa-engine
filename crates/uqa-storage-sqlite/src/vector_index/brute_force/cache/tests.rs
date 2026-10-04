//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection};
use uqa_storage::{mvcc::VersionedSessionOptions, read_control::StorageReadControl, VectorIndex};

fn fixture() -> (ManagedConnection, SQLiteVectorIndex) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut index = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 2);
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    (connection, index)
}

fn nearest(index: &SQLiteVectorIndex, query: &[f32]) -> Vec<DocId> {
    index.search_knn(query, 1).unwrap().doc_ids().collect()
}

#[test]
fn exact_cache_keeps_committed_private_and_rolled_back_vector_generations() {
    let (connection, mut index) = fixture();
    assert_eq!(nearest(&index, &[1.0, 0.0]), [1]);
    let first = index
        .cached_vectors
        .read()
        .as_ref()
        .unwrap()
        .coordinates
        .clone();
    assert_eq!(nearest(&index, &[0.0, 1.0]), [2]);
    assert!(std::ptr::eq(
        &raw const *first,
        &raw const *index.cached_vectors.read().as_ref().unwrap().coordinates
    ));
    let stable = index.retained_snapshot().unwrap();
    connection.begin_transaction().unwrap();
    connection.savepoint("before").unwrap();
    index.add(3, vec![-1.0, 0.0]).unwrap();
    assert_eq!(nearest(&index, &[-1.0, 0.0]), [3]);
    let branch = index.retained_snapshot().unwrap();
    connection.rollback_to_savepoint("before").unwrap();
    index.add(4, vec![-1.0, 0.0]).unwrap();
    assert_eq!(nearest(&index, &[-1.0, 0.0]), [4]);
    assert_eq!(nearest(&branch, &[-1.0, 0.0]), [3]);
    connection.rollback_transaction().unwrap();
    let mut writer = SQLiteVectorIndex::new(connection.new_session(), "docs", "embedding", 2);
    writer.add(5, vec![-1.0, 0.0]).unwrap();
    assert_eq!(nearest(&index, &[-1.0, 0.0]), [5]);
    assert_eq!(nearest(&stable, &[1.0, 0.0]), [1]);
    assert_eq!(nearest(&index, &[-1.0, 0.0]), [5]);
    writer.delete(5).unwrap();
    writer
        .add_many(1, vec![vec![-1.0, 0.0], vec![1.0, 0.0]])
        .unwrap();
    assert_eq!(nearest(&index, &[-1.0, 0.0]), [1]);
    assert_eq!(nearest(&index, &[1.0, 0.0]), [1]);
    writer.clear().unwrap();
    assert!(nearest(&index, &[1.0, 0.0]).is_empty());
}

#[test]
fn exact_cache_readers_bind_current_cancellation_and_release_their_allowance() {
    let (connection, mut index) = fixture();
    let mut snapshot = std::sync::Arc::try_unwrap(connection.native_snapshot().unwrap().unwrap())
        .ok()
        .unwrap();
    let first = StorageReadControl::with_limit(65536);
    snapshot.control = first.clone();
    index.retained = Some(std::sync::Arc::new(snapshot));
    assert_eq!(nearest(&index, &[1.0, 0.0]), [1]);
    let held = index
        .cached_vectors
        .read()
        .as_ref()
        .unwrap()
        .coordinates
        .clone();
    assert!(first.memory().used() > 0);
    first.cancellation().cancel();
    assert!(matches!(
        index.search_knn(&[1.0, 0.0], 1),
        Err(StorageBackendError::Cancelled(_))
    ));
    let mut current = index.clone();
    current.retained = connection.native_snapshot().unwrap();
    assert_eq!(nearest(&current, &[1.0, 0.0]), [1]);
    drop((index, current, connection));
    assert!(first.memory().used() > 0);
    drop(held);
    assert_eq!(first.memory().used(), 0);
}

#[test]
fn exact_cache_matches_streaming_scores_and_does_not_retain_an_oversized_corpus() {
    let (connection, mut index) = fixture();
    connection.begin_transaction().unwrap();
    for id in 3..=512 {
        index.add(id, vec![id as f32, 1.0]).unwrap();
    }
    connection.commit_transaction().unwrap();
    let mut snapshot = std::sync::Arc::try_unwrap(connection.native_snapshot().unwrap().unwrap())
        .ok()
        .unwrap();
    let control = StorageReadControl::with_limit(65536);
    snapshot.control = control.clone();
    index.retained = Some(std::sync::Arc::new(snapshot));
    let query = [1.0, -1.0];
    let actual = index.search_threshold(&query, -1.0).unwrap();
    assert!(index.cached_vectors.read().is_none());
    assert_eq!(control.memory().used(), 0);
    let expected = actual
        .entries()
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
        .collect::<Vec<_>>();
    let mut current = index.clone();
    current.retained = connection.native_snapshot().unwrap();
    for _ in 0..2 {
        let actual = current.search_threshold(&query, -1.0).unwrap();
        assert_eq!(
            actual
                .entries()
                .iter()
                .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert!(current.cached_vectors.read().is_some());
}

#[test]
fn optional_exact_cache_yields_to_the_original_streaming_workspace() {
    let (connection, _) = fixture();
    let mut index = SQLiteVectorIndex::new(connection.clone(), "docs", "wide", 128);
    connection.begin_transaction().unwrap();
    for id in 1..=12 {
        index.add(id, vec![1.0; 128]).unwrap();
    }
    connection.commit_transaction().unwrap();
    let mut snapshot = std::sync::Arc::try_unwrap(connection.native_snapshot().unwrap().unwrap())
        .ok()
        .unwrap();
    let control = StorageReadControl::with_limit(131_072);
    snapshot.control = control.clone();
    index.retained = Some(std::sync::Arc::new(snapshot));
    let query = vec![1.0; 128];
    {
        let snapshot = index.native_snapshot().unwrap().unwrap();
        let read = NativeVectorRead::new(&snapshot, &index).unwrap();
        let scores =
            SQLiteVectorIndex::score_stream(&query, None, |visit| read.visit_vectors(visit))
                .unwrap();
        assert_eq!(scores.unwrap().len(), 12);
    }
    assert_eq!(control.memory().used(), 0);
    let streaming_peak = control.memory().peak();
    assert_eq!(nearest(&index, &query), [1]);
    let retained_bytes = control.memory().used();
    assert!(retained_bytes > 0);
    let hold = control
        .memory()
        .reserve(control.memory().limit() - streaming_peak.max(retained_bytes))
        .unwrap();
    assert_eq!(nearest(&index, &query), [1]);
    drop(hold);
    assert!(index.cached_vectors.read().is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn optional_exact_cache_yields_to_posting_construction() {
    let (connection, mut index) = fixture();
    let mut snapshot = std::sync::Arc::try_unwrap(connection.native_snapshot().unwrap().unwrap())
        .ok()
        .unwrap();
    let control = StorageReadControl::with_limit(65536);
    snapshot.control = control.clone();
    index.retained = Some(std::sync::Arc::new(snapshot));
    let scores = index
        .score_vectors(&[1.0, 0.0], Some(-1.0))
        .unwrap()
        .unwrap();
    assert!(index.cached_vectors.read().is_some());
    let (scores, score_memory) = scores.into_parts();
    let mut output = BudgetedVec::<super::super::PostingEntry>::new(control.memory());
    output.reserve(scores.len()).unwrap();
    let output_bytes = output.capacity() * std::mem::size_of::<super::super::PostingEntry>();
    drop(output);
    let hold = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used() - output_bytes + 1)
        .unwrap();
    let postings = index.score_postings(scores, score_memory.budget()).unwrap();
    assert_eq!(postings.doc_ids().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(postings.entries()[0].payload.score, 1.0);
    assert_eq!(postings.entries()[1].payload.score, 0.0);
    assert!(index.cached_vectors.read().is_none());
    drop((hold, score_memory));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn exact_cache_never_publishes_invalid_canonical_ordinals() {
    use rusqlite::types::ValueRef;

    let (connection, index) = fixture();
    assert_eq!(nearest(&index, &[1.0, 0.0]), [1]);
    let stable = index.retained_snapshot().unwrap();
    connection.begin_transaction().unwrap();
    index
        .write_native(|read, batch| {
            let vector = super::super::vector_to_blob(&[1.0, 0.0])?;
            read.snapshot.put_row(
                batch,
                Family::Vectors,
                read.owner.unwrap(),
                &[
                    ValueRef::Text(b"docs"),
                    read.field(),
                    ValueRef::Integer(3),
                    ValueRef::Integer(1),
                    ValueRef::Blob(&vector),
                ],
            )
        })
        .unwrap();
    for _ in 0..2 {
        assert!(index.search_knn(&[1.0, 0.0], 1).is_err());
        assert!(index.cached_vectors.read().is_none());
    }
    assert_eq!(nearest(&stable, &[1.0, 0.0]), [1]);
    connection.commit_transaction().unwrap();
    assert!(index.search_knn(&[1.0, 0.0], 1).is_err());
    assert!(index.cached_vectors.read().is_none());
    assert_eq!(nearest(&stable, &[1.0, 0.0]), [1]);
}
