//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared command-boundary schedules for independently opened persistent sessions.

use std::{collections::BTreeMap, sync::Arc};

use super::VectorMergeKind;
use crate::{
    InvertedIndex, KeyValueInvertedIndex, KeyValueStorageBackend, KeyValueStore,
    PersistentStorageBackend, StorageBackendResult, StorageSavepointId,
};

const OCCURRENCES: &str = "command_occurrences";

fn fields(length: usize) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), vec!["alpha"; length].join(" "))])
}

/// Verify that successive commands merge private occurrence/vector changes with independent commits, including savepoint undo and retained readers. Requires two fresh disposable sessions over the same database.
pub fn verify_command_refresh(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    occurrences(a, b)?;
    for kind in [VectorMergeKind::IVF, VectorMergeKind::HNSW] {
        vectors(a, b, kind)?;
    }
    Ok(())
}

fn occurrences(a: &Arc<dyn KeyValueStore>, b: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let backend = KeyValueStorageBackend::new(a.clone());
    let mut left =
        KeyValueInvertedIndex::new(a.clone(), OCCURRENCES, uqa_analysis::whitespace_analyzer());
    let mut right =
        KeyValueInvertedIndex::new(b.clone(), OCCURRENCES, uqa_analysis::whitespace_analyzer());
    left.add_document(1, fields(1))?;
    backend.begin_transaction()?;
    left.add_document(2, fields(2))?;
    let before = StorageSavepointId::allocate();
    backend.savepoint(before)?;
    let retained = left.snapshot()?;
    right.add_document(3, fields(3))?;
    backend.refresh_transaction_snapshot(&uqa_core::CancellationToken::new())?;
    assert_eq!(left.total_field_length("body")?, 6);
    left.remove_document(2)?;
    right.add_document(4, fields(4))?;
    backend.refresh_transaction_snapshot(&uqa_core::CancellationToken::new())?;
    assert_eq!(left.total_field_length("body")?, 8);
    backend.rollback_to_savepoint(before)?;
    assert_eq!(left.total_field_length("body")?, 3);
    backend.refresh_transaction_snapshot(&uqa_core::CancellationToken::new())?;
    assert_eq!(left.total_field_length("body")?, 10);
    backend.commit_transaction()?;
    assert_eq!(retained.total_field_length("body")?, 3);
    assert_eq!(right.total_field_length("body")?, 10);
    assert_eq!(right.doc_count()?, 4);
    Ok(())
}

fn vectors(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
    kind: VectorMergeKind,
) -> StorageBackendResult<()> {
    let table = format!("command_{kind:?}");
    let backend = KeyValueStorageBackend::new(a.clone());
    let mut left = kind.open(a.clone(), &table, false, false)?;
    left.add(1, vec![1.0, 0.0])?;
    left.initialize()?;
    let mut right = kind.open(b.clone(), &table, true, false)?;
    backend.begin_transaction()?;
    left.add(2, vec![0.0, 1.0])?;
    let before = StorageSavepointId::allocate();
    backend.savepoint(before)?;
    let retained = left.snapshot()?;
    right.add(3, vec![0.5, 0.5])?;
    backend.refresh_transaction_snapshot(&uqa_core::CancellationToken::new())?;
    assert_eq!(left.count()?, 3);
    left.add(4, vec![0.25, 0.75])?;
    right.add(5, vec![0.75, 0.25])?;
    backend.refresh_transaction_snapshot(&uqa_core::CancellationToken::new())?;
    assert_eq!(left.count()?, 5);
    backend.rollback_to_savepoint(before)?;
    assert_eq!(left.count()?, 2);
    backend.refresh_transaction_snapshot(&uqa_core::CancellationToken::new())?;
    assert_eq!(left.count()?, 4);
    right.add(6, vec![0.9, 0.1])?;
    backend.commit_transaction()?;
    assert_eq!(retained.count()?, 2);
    assert_eq!(right.count()?, 5);
    assert_eq!(right.search_knn(&[1.0, 0.0], 100)?.len(), 5);
    Ok(())
}

/// Verify durable results after closing all sessions used by [`verify_command_refresh`] and reopening the database.
pub fn verify_command_refresh_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    for kind in [VectorMergeKind::IVF, VectorMergeKind::HNSW] {
        let index = kind.open(store.clone(), &format!("command_{kind:?}"), true, false)?;
        assert_eq!(index.count()?, 5);
        assert_eq!(index.search_knn(&[1.0, 0.0], 100)?.len(), 5);
    }
    let index = KeyValueInvertedIndex::new(store, OCCURRENCES, uqa_analysis::whitespace_analyzer());
    assert_eq!(index.total_field_length("body")?, 10);
    assert_eq!(index.doc_count()?, 4);
    Ok(())
}
