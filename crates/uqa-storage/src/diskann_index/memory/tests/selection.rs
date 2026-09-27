//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::DiskANNReadChanges;

fn selection(
    index: &dyn VectorIndex,
    ids: &[DocId],
    control: &StorageReadControl,
) -> DiskANNReadChanges {
    let source = index.diskann_read_snapshot(control).unwrap().unwrap();
    DiskANNReadChanges::capture(
        ids.iter().map(|id| Ok((*id, Some(source.clone())))),
        control,
    )
    .unwrap()
}

#[test]
fn diskann_selected_private_origins_preserve_fixed_rows_and_tensor_scores_after_compaction() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut base = new(&control);
    base.add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    base.add(2, vec![0.0, 1.0]).unwrap();
    base.add(3, vec![-1.0, 0.0]).unwrap();
    base.add(4, vec![0.0, 0.0]).unwrap();
    base.initialize().unwrap();
    base.add(7, vec![1.0, 0.0]).unwrap();
    let original = scores(&base);
    let mut private = base.fork().unwrap();
    private.add(1, vec![-1.0, 0.0]).unwrap();
    private.delete(2).unwrap();
    private
        .add_many(5, vec![vec![-1.0, 0.0], vec![1.0, 0.0]])
        .unwrap();
    // These later committed changes are outside the evaluated private selection.
    private.add(3, vec![1.0, 0.0]).unwrap();
    private.add(4, vec![0.0, 1.0]).unwrap();
    private.initialize().unwrap();
    let changes = selection(&private, &[1, 2, 5], &control);
    let projected = base
        .snapshot_with_diskann_changes(&changes, &control)
        .unwrap()
        .unwrap();
    assert_eq!(projected.index_kind(), "diskann");
    assert_eq!(
        scores(&*projected),
        [(1, -1.0), (3, -1.0), (4, 0.0), (5, 1.0), (7, 1.0)]
    );
    assert_eq!(projected.count().unwrap(), 6);
    assert!(!projected.contains_document(2).unwrap());
    assert!(projected.contains_document(5).unwrap());
    assert_eq!(
        projected
            .search_threshold(&[1.0, 0.0], 0.0)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [4, 5, 7]
    );
    assert_eq!(
        projected
            .search_knn(&[1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [5]
    );
    assert_eq!(scores(&base), original);
    let nested = projected.snapshot().unwrap().snapshot().unwrap();
    drop((projected, changes, private, base));
    assert_eq!(
        scores(&*nested),
        [(1, -1.0), (3, -1.0), (4, 0.0), (5, 1.0), (7, 1.0)]
    );
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_selected_sources_keep_each_evaluated_version_and_absent_terminal_identity() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut base = new(&control);
    base.add(1, vec![1.0, 0.0]).unwrap();
    base.add(2, vec![0.0, 1.0]).unwrap();
    base.add(3, vec![1.0, 0.0]).unwrap();
    base.add(DocId::MAX, vec![1.0, 0.0]).unwrap();
    base.initialize().unwrap();
    let mut private = base.fork().unwrap();
    private.add(1, vec![-1.0, 0.0]).unwrap();
    let first = private.diskann_read_snapshot(&control).unwrap().unwrap();
    private.add(1, vec![0.0, 1.0]).unwrap();
    private.add(2, vec![1.0, 0.0]).unwrap();
    let second = private.diskann_read_snapshot(&control).unwrap().unwrap();
    private.clear().unwrap();
    let absent = private.diskann_read_snapshot(&control).unwrap().unwrap();
    let changes = DiskANNReadChanges::capture(
        [
            Ok((1, Some(first))),
            Ok((2, Some(second))),
            Ok((3, None)),
            Ok((DocId::MAX, Some(absent))),
        ],
        &control,
    )
    .unwrap();
    let projected = base
        .snapshot_with_diskann_changes(&changes, &control)
        .unwrap()
        .unwrap();
    assert_eq!(scores(&*projected), [(1, -1.0), (2, 1.0)]);
    assert_eq!(projected.count().unwrap(), 2);
    assert!(!projected.contains_document(DocId::MAX).unwrap());
    assert_eq!(
        projected
            .search_threshold(&[1.0, 0.0], -1.0)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [1, 2]
    );
    private.add(1, vec![0.0, 0.0]).unwrap();
    let newer = selection(&private, &[1], &control);
    let nested = projected
        .snapshot_with_diskann_changes(&newer, &control)
        .unwrap()
        .unwrap();
    assert_eq!(scores(&*nested), [(1, 0.0), (2, 1.0)]);
    assert_eq!(scores(&*projected), [(1, -1.0), (2, 1.0)]);
}

#[test]
fn diskann_private_selection_rejects_unrelated_lineages_and_unordered_identities() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut base = new(&control);
    base.add(1, vec![1.0, 0.0]).unwrap();
    let other = new(&control);
    let changes = selection(&other, &[1], &control);
    let used = control.memory().used();
    assert!(base
        .snapshot_with_diskann_changes(&changes, &control)
        .is_err());
    assert_eq!(control.memory().used(), used);
    assert_eq!(scores(&base), [(1, 1.0)]);
    let source = base.diskann_read_snapshot(&control).unwrap().unwrap();
    for ids in [[2, 1], [1, 1]] {
        assert!(DiskANNReadChanges::capture(
            ids.map(|id| Ok((id, Some(source.clone())))),
            &control
        )
        .is_err());
        assert_eq!(control.memory().used(), used);
    }
}

#[test]
fn diskann_complete_selection_masks_unselected_base_and_journal_documents() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = new(&control);
    for id in [1, 2, 3, DocId::MAX] {
        index.add(id, vec![1.0, 0.0]).unwrap();
    }
    index.initialize().unwrap();
    index.add(1, vec![-1.0, 0.0]).unwrap();
    index
        .add_many(4, vec![vec![0.0, 1.0], vec![1.0, 0.0]])
        .unwrap();
    let source = index.diskann_read_snapshot(&control).unwrap().unwrap();
    let changes = DiskANNReadChanges::capture(
        [
            Ok((1, Some(source.clone()))),
            Ok((3, None)),
            Ok((4, Some(source))),
        ],
        &control,
    )
    .unwrap()
    .without_unselected_documents();
    index.add(1, vec![0.0, 1.0]).unwrap();
    index.add(5, vec![1.0, 0.0]).unwrap();
    index.initialize().unwrap();
    index.add(6, vec![1.0, 0.0]).unwrap();
    let projected = index
        .snapshot_with_diskann_changes(&changes, &control)
        .unwrap()
        .unwrap();
    assert_eq!(projected.index_kind(), "diskann");
    assert_eq!(scores(&*projected), [(1, -1.0), (4, 1.0)]);
    assert_eq!(projected.count().unwrap(), 3);
    for id in [2, 3, 5, 6, DocId::MAX] {
        assert!(!projected.contains_document(id).unwrap());
    }
    assert_eq!(
        projected
            .search_threshold(&[1.0, 0.0], 0.0)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [4]
    );
    let newer = selection(&index, &[2], &control);
    let nested = projected
        .snapshot_with_diskann_changes(&newer, &control)
        .unwrap()
        .unwrap();
    assert_eq!(scores(&*nested), [(1, -1.0), (2, 1.0), (4, 1.0)]);
    let empty_changes = DiskANNReadChanges::capture([], &control)
        .unwrap()
        .without_unselected_documents();
    let empty = index
        .snapshot_with_diskann_changes(&empty_changes, &control)
        .unwrap()
        .unwrap();
    assert!(scores(&*empty).is_empty());
    assert_eq!(empty.count().unwrap(), 0);
    let retained = projected.snapshot().unwrap();
    drop((
        index,
        projected,
        changes,
        newer,
        nested,
        empty,
        empty_changes,
    ));
    assert_eq!(scores(&*retained), [(1, -1.0), (4, 1.0)]);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_private_selection_reserves_original_allowances_and_releases_failures() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut base = new(&control);
    base.add(1, vec![1.0, 0.0]).unwrap();
    let source = base.diskann_read_snapshot(&control).unwrap().unwrap();
    let tiny = StorageReadControl::with_limit(1);
    assert!(DiskANNReadChanges::capture([Ok((1, Some(source)))], &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let changes = selection(&base, &[1], &control);
    let used = control.memory().used();
    let hold = control.memory().reserve((1 << 20) - used).unwrap();
    let fresh = StorageReadControl::with_limit(1 << 22);
    assert!(base
        .snapshot_with_diskann_changes(&changes, &fresh)
        .is_err());
    assert_eq!(fresh.memory().used(), 0);
    drop(hold);
    assert_eq!(control.memory().used(), used);
    let projected = base
        .snapshot_with_diskann_changes(&changes, &fresh)
        .unwrap()
        .unwrap();
    let retained_bytes = control.memory().used();
    let hold = control
        .memory()
        .reserve((1 << 20) - retained_bytes)
        .unwrap();
    assert!(projected
        .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
        .is_err());
    assert!(projected
        .search_threshold_with_control(&[1.0, 0.0], 0.0, &fresh)
        .is_err());
    assert!(projected
        .search_knn_with_control(&[1.0, 0.0], 0, &fresh)
        .unwrap()
        .is_empty());
    assert_eq!(fresh.memory().used(), 0);
    drop(hold);
    assert_eq!(control.memory().used(), retained_bytes);
    fresh.cancellation().cancel();
    assert!(projected.snapshot_with_control(&fresh).is_err());
    assert!(projected
        .search_knn_with_control(&[1.0, 0.0], 0, &fresh)
        .is_err());
    assert_eq!(scores(&*projected), [(1, 1.0)]);
    control.cancellation().cancel();
    assert!(projected.count().is_err());
    assert!(projected.contains_document(1).is_err());
    assert!(projected.search_knn(&[1.0, 0.0], 0).is_err());
    drop((projected, changes, base));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_private_selection_preserves_read_only_source_and_receiver_guards() {
    use crate::ReadOnlySnapshot;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut base = new(&control);
    base.add(1, vec![1.0, 0.0]).unwrap();
    let receiver_control = StorageReadControl::with_limit(8192);
    let source_control = StorageReadControl::with_limit(8192);
    let receiver = ReadOnlySnapshot::new(base.snapshot().unwrap())
        .with_vector_read_control(&receiver_control)
        .unwrap();
    let source = ReadOnlySnapshot::with_retention(
        base.snapshot().unwrap(),
        source_control.memory().reserve(128).unwrap(),
    )
    .unwrap()
    .with_vector_read_control(&source_control)
    .unwrap();
    let changes = selection(&source, &[1], &control);
    let projected = receiver
        .snapshot_with_diskann_changes(&changes, &control)
        .unwrap()
        .unwrap();
    drop(source);
    assert!(source_control.memory().used() >= 128);
    assert_eq!(scores(&*projected), [(1, 1.0)]);
    source_control.cancellation().cancel();
    assert!(projected.count().is_err());
    assert!(projected.search_threshold(&[1.0, 0.0], 0.0).is_err());
    receiver_control.cancellation().cancel();
    assert!(projected.search_knn(&[1.0, 0.0], 0).is_err());
    assert!(receiver.diskann_read_snapshot(&control).is_err());
    drop((projected, changes, receiver, base));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source_control.memory().used(), 0);
}
