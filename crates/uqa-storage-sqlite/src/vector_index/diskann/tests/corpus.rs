//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::{
    build::{DiskANNBuildInput, DiskANNTemporaryBudget},
    format::DiskANNGeneration,
};
use uqa_storage::VectorIndex;

#[test]
fn native_diskann_canonical_corpus_captures_actual_origins_and_reopens_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    for mode in 0..4 {
        let path = directory.path().join(format!("corpus-{mode}.db"));
        let (retained, low, high, empty) = {
            let connection = open(&path, mode);
            let index = canonical(&connection, "docs", "embedding", 2);
            let absent = index.retain(&control).unwrap();
            let high = index
                .replace(i64::MAX as DocId, &[vec![f32::MAX, 0.0]], &control)
                .unwrap();
            let empty = index.replace(7, &[], &control).unwrap();
            let low = index
                .replace(0, &[vec![3.0, 4.0], vec![-0.0, 0.0]], &control)
                .unwrap();
            assert_eq!(absent.next_document_after(None, &control).unwrap(), None);
            let retained = index.retain(&control).unwrap();
            connection.begin_transaction().unwrap();
            index.replace(0, &[], &control).unwrap();
            index.replace(3, &[vec![1.0, 0.0]], &control).unwrap();
            let private = index.retain(&control).unwrap();
            connection.rollback_transaction().unwrap();
            let mut docs = Vec::new();
            private
                .visit_all(&control, &mut |doc, _, _, _| {
                    docs.push(doc);
                    Ok(())
                })
                .unwrap();
            assert_eq!(docs, [3, i64::MAX as DocId]);
            (retained, low, high, empty)
        };
        assert_corpus(&retained, low, high, empty, scratch.path());
        drop(retained);
        let reopened = open(&path, mode);
        let retained = canonical(&reopened, "docs", "embedding", 2)
            .retain(&control)
            .unwrap();
        assert_corpus(&retained, low, high, empty, scratch.path());
    }
}

fn assert_corpus(
    retained: &RetainedSQLiteDiskANNCanonical,
    low: DiskANNVectorVersion,
    high: DiskANNVectorVersion,
    empty: DiskANNVectorVersion,
    directory: &Path,
) {
    let query = StorageReadControl::with_limit(8192);
    assert_eq!(retained.dimensions(), 2);
    let mut after = None;
    for doc in [0, 7, i64::MAX as DocId] {
        assert_eq!(
            retained.next_document_after(after, &query).unwrap(),
            Some(doc)
        );
        after = Some(doc);
    }
    assert_eq!(retained.next_document_after(after, &query).unwrap(), None);
    assert_eq!(
        retained
            .next_document_after(Some(DocId::MAX), &query)
            .unwrap(),
        None
    );
    assert_eq!(retained.origin(7, &query).unwrap(), Some(empty));
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = DiskANNBuildInput::capture_source(
        DiskANNGeneration::new([19; 16], 1, 2, 3).unwrap(),
        retained,
        directory,
        &temporary,
        &query,
    )
    .unwrap();
    assert_eq!((input.node_count(), input.side_count()), (1, 2));
    assert_eq!(input.coverage().vector_count(), 3);
    let node = input.read_node(0).unwrap();
    assert_eq!((node.doc_id(), node.ordinal(), node.version()), (0, 0, low));
    assert_eq!(node.raw(), [3.0, 4.0]);
    drop(node);
    let zero = input.read_side(0).unwrap();
    assert_eq!((zero.doc_id(), zero.ordinal(), zero.version()), (0, 1, low));
    assert_eq!(zero.raw()[0].to_bits(), 0x8000_0000);
    drop(zero);
    let overflow = input.read_side(1).unwrap();
    assert_eq!(
        (overflow.doc_id(), overflow.ordinal(), overflow.version()),
        (i64::MAX as DocId, 0, high)
    );
    drop(overflow);
    drop(input);
    assert_eq!(temporary.used(), 0);
    assert_eq!(query.memory().used(), 0);
    assert!(std::fs::read_dir(directory).unwrap().next().is_none());
}

#[test]
fn native_diskann_canonical_corpus_keeps_controls_and_rejects_unstamped_values() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let index = canonical(&connection, "docs", "embedding", 2);
    index
        .replace(0, &[vec![3.0, 4.0], vec![1.0, 0.0]], &control)
        .unwrap();
    index.replace(7, &[], &control).unwrap();
    let retained = index.retain(&control).unwrap();
    let tiny = StorageReadControl::with_limit(1);
    assert!(retained.next_document_after(None, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let query = StorageReadControl::with_limit(8192);
    let mut calls = 0;
    assert!(retained
        .visit_all(&query, &mut |_, _, _, _| {
            calls += 1;
            query.cancellation().cancel();
            Ok(())
        })
        .is_err());
    assert_eq!(calls, 1);
    assert_eq!(query.memory().used(), 0);
    let query = StorageReadControl::with_limit(8192);
    let mut legacy = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 2);
    legacy.add(1, vec![1.0, 0.0]).unwrap();
    let unstamped = index.retain(&control).unwrap();
    assert_eq!(
        unstamped.next_document_after(Some(0), &query).unwrap(),
        Some(1)
    );
    let scratch = tempfile::tempdir().unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    assert!(DiskANNBuildInput::capture_source(
        DiskANNGeneration::new([19; 16], 1, 2, 3).unwrap(),
        &unstamped,
        scratch.path(),
        &temporary,
        &query,
    )
    .is_err());
    assert_eq!(temporary.used(), 0);
    assert_eq!(query.memory().used(), 0);
    assert!(std::fs::read_dir(scratch.path()).unwrap().next().is_none());
    control.cancellation().cancel();
    assert!(retained
        .next_document_after(Some(DocId::MAX), &query)
        .is_err());
}
