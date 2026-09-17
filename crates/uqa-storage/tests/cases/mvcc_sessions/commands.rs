//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command refresh retains evaluated private effects, fixed readers and matching savepoint bases.

use super::*;
use uqa_storage::{CatalogFacade, InvertedIndex, KeyValueCatalog, KeyValueInvertedIndex};

#[test]
fn shared_command_refresh_contract_uses_the_paired_backend_boundary() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let b: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_command_refresh(&a, &b).unwrap();
    drop((a, b));
    uqa_storage::key_value::conformance::verify_command_refresh_reopen(Arc::new(
        persistence.session(1 << 22),
    ))
    .unwrap();
}

#[test]
fn command_refresh_preserves_private_rows_and_duplicate_savepoint_boundaries() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.put(b"row", b"original").unwrap();
    a.begin_transaction().unwrap();
    a.put(b"row", b"first").unwrap();
    a.savepoint("same").unwrap();
    let retained = a.record_snapshot().unwrap();
    b.put(b"peer", b"first peer").unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(a.get(b"row").unwrap().unwrap(), b"first");
    assert_eq!(a.get(b"peer").unwrap().unwrap(), b"first peer");
    let control = a.retention_control();
    assert!(retained.get(b"peer", &control).unwrap().is_none());
    a.savepoint("same").unwrap();
    a.put(b"row", b"second").unwrap();
    b.put(b"peer", b"second peer").unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    let discarded = a.record_snapshot().unwrap();
    a.rollback_to_savepoint("same").unwrap();
    assert_eq!(a.get(b"row").unwrap().unwrap(), b"first");
    assert_eq!(a.get(b"peer").unwrap().unwrap(), b"first peer");
    a.release_savepoint("same").unwrap();
    a.rollback_to_savepoint("same").unwrap();
    assert!(a.get(b"peer").unwrap().is_none());
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(a.get(b"peer").unwrap().unwrap(), b"second peer");
    a.put(b"row", b"final").unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(b.get(b"row").unwrap().unwrap(), b"final");
    assert_eq!(b.get(b"peer").unwrap().unwrap(), b"second peer");
    assert_eq!(
        discarded.get(b"row", &control).unwrap().unwrap().value(),
        Some(b"second".as_slice())
    );
    drop((retained, discarded));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_command_refresh_does_not_advance_or_discard_private_changes() {
    for requirement in [false, true] {
        let persistence = Persistence::new();
        let a = persistence.session(1 << 20);
        let b = persistence.session(1 << 20);
        a.put(b"shared", b"original").unwrap();
        a.begin_transaction().unwrap();
        a.put(b"private", b"kept").unwrap();
        if requirement {
            a.with_mutation(&mut |_, batch| batch.require_unchanged(b"shared"))
                .unwrap();
        } else {
            a.put(b"shared", b"mine").unwrap();
        }
        b.put(b"shared", b"winner").unwrap();
        b.put(b"late", b"hidden").unwrap();
        let before = a.scan_prefix(b"").unwrap();
        let error = a
            .refresh_transaction_snapshot(a.retention_control().cancellation())
            .unwrap_err();
        let StorageBackendError::Backend { source, .. } = error else {
            panic!("expected a typed MVCC conflict");
        };
        assert!(matches!(
            source.downcast_ref::<VersionError>(),
            Some(VersionError::ReadConflict { .. } | VersionError::WriteConflict { .. })
        ));
        assert_eq!(a.scan_prefix(b"").unwrap(), before);
        assert!(a.pending_commit().is_none());
        a.rollback_transaction().unwrap();
        assert_eq!(a.get(b"shared").unwrap().unwrap(), b"winner");
    }
}

#[test]
fn command_refresh_respects_read_only_snapshots_and_retained_allowances() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    assert!(a
        .refresh_transaction_snapshot(a.retention_control().cancellation())
        .is_err());
    a.begin_read_transaction().unwrap();
    b.put(b"peer", b"visible after refresh").unwrap();
    let control = a.retention_control();
    let occupied = control.memory().used();
    let hold = control.memory().reserve((1 << 20) - occupied).unwrap();
    assert!(matches!(
        a.refresh_transaction_snapshot(a.retention_control().cancellation()),
        Err(StorageBackendError::Memory(_))
    ));
    drop(hold);
    assert!(a.get(b"peer").unwrap().is_none());
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(a.get(b"peer").unwrap().unwrap(), b"visible after refresh");
    assert!(a.put(b"private", b"forbidden").is_err());
    a.commit_transaction().unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn cancelled_or_sealed_transactions_cannot_advance_their_command_view() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.begin_transaction().unwrap();
    a.put(b"private", b"kept").unwrap();
    b.put(b"peer", b"late").unwrap();
    let before = a.scan_prefix(b"").unwrap();
    let cancellation = uqa_core::CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        a.refresh_transaction_snapshot(&cancellation),
        Err(StorageBackendError::Cancelled(_))
    ));
    cancellation.reset();
    assert_eq!(a.scan_prefix(b"").unwrap(), before);
    persistence.state.lock().commit_fault = CommitFault::Reject;
    assert!(a.commit_transaction().is_err());
    let attempt = a.pending_commit().unwrap();
    let StorageBackendError::Backend { source, .. } = a
        .refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap_err()
    else {
        panic!("expected a sealed transaction error");
    };
    assert!(matches!(
        source.downcast_ref::<VersionError>(),
        Some(VersionError::TransactionSealed)
    ));
    assert_eq!(a.pending_commit(), Some(attempt));
    assert_eq!(a.scan_prefix(b"").unwrap(), before);
    persistence.state.lock().commit_fault = CommitFault::None;
    a.commit_transaction().unwrap();
    assert_eq!(b.get(b"private").unwrap().unwrap(), b"kept");
    assert_eq!(b.get(b"peer").unwrap().unwrap(), b"late");
}

fn fields(length: usize) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), vec!["alpha"; length].join(" "))])
}

#[test]
fn occurrence_refresh_rebases_each_command_and_savepoint_without_double_counting() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let b = Arc::new(persistence.session(1 << 22));
    let mut left =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let mut right = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    left.add_document(1, fields(1)).unwrap();
    a.begin_transaction().unwrap();
    left.add_document(2, fields(2)).unwrap();
    a.savepoint("before").unwrap();
    let retained = left.snapshot().unwrap();
    right.add_document(3, fields(3)).unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 6);
    assert_eq!(retained.total_field_length("body").unwrap(), 3);
    left.add_document(4, fields(4)).unwrap();
    a.savepoint("after").unwrap();
    right.add_document(5, fields(5)).unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 15);
    a.rollback_to_savepoint("after").unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 10);
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 15);
    a.rollback_to_savepoint("before").unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 3);
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 11);
    left.remove_document(2).unwrap();
    right.add_document(6, fields(6)).unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(right.total_field_length("body").unwrap(), 15);
    assert_eq!(right.doc_count().unwrap(), 4);
    assert_eq!(retained.total_field_length("body").unwrap(), 3);
}

#[test]
fn vector_refresh_retains_input_journals_across_commands_and_undo() {
    for hnsw in [false, true] {
        let persistence = Persistence::new();
        let (a, _, mut left, mut right) = super::vector_merging::fixture(&persistence, hnsw);
        a.begin_transaction().unwrap();
        left.add(2, vec![0.0, 1.0]).unwrap();
        a.savepoint("before").unwrap();
        let retained = left.snapshot().unwrap();
        right.add(3, vec![0.5, 0.5]).unwrap();
        a.refresh_transaction_snapshot(a.retention_control().cancellation())
            .unwrap();
        assert_eq!(left.count().unwrap(), 3);
        assert_eq!(left.search_knn(&[1.0, 0.0], 100).unwrap().len(), 3);
        left.add_many(4, vec![vec![0.25, 0.75], vec![0.75, 0.25]])
            .unwrap();
        a.savepoint("after").unwrap();
        right.add(5, vec![0.9, 0.1]).unwrap();
        a.refresh_transaction_snapshot(a.retention_control().cancellation())
            .unwrap();
        assert_eq!(left.count().unwrap(), 6);
        a.rollback_to_savepoint("after").unwrap();
        assert_eq!(left.count().unwrap(), 5);
        a.refresh_transaction_snapshot(a.retention_control().cancellation())
            .unwrap();
        assert_eq!(left.count().unwrap(), 6);
        a.rollback_to_savepoint("before").unwrap();
        assert_eq!(left.count().unwrap(), 2);
        a.refresh_transaction_snapshot(a.retention_control().cancellation())
            .unwrap();
        assert_eq!(left.count().unwrap(), 4);
        left.add(6, vec![0.1, 0.9]).unwrap();
        right.add(7, vec![0.3, 0.7]).unwrap();
        a.commit_transaction().unwrap();
        assert_eq!(right.count().unwrap(), 6);
        assert_eq!(right.search_knn(&[1.0, 0.0], 100).unwrap().len(), 6);
        assert_eq!(retained.count().unwrap(), 2);
    }
}

#[test]
fn graph_and_occurrence_command_refresh_keep_late_caches_invalid() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let b = Arc::new(persistence.session(1 << 22));
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b.clone());
    first.save_named_graph("g").unwrap();
    first.save_vertex(1, "node", "{}").unwrap();
    first.save_graph_membership("vertex", 1, "g").unwrap();
    let mut left =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let mut right = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    left.add_document(1, fields(1)).unwrap();
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    left.add_document(2, fields(2)).unwrap();
    right.add_document(3, fields(3)).unwrap();
    second.save_path_index("late", "[]").unwrap();
    second.finish_path_index_data("late", "g", "[]").unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(left.total_field_length("body").unwrap(), 6);
    assert!(!first.path_index_data_is_current("late", "[]").unwrap());
    first.finish_path_index_data("late", "g", "[]").unwrap();
    assert!(first.path_index_data_is_current("late", "[]").unwrap());
    second.save_vertex(2, "node", "{}").unwrap();
    second.save_graph_membership("vertex", 2, "g").unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert!(!first.path_index_data_is_current("late", "[]").unwrap());
    a.commit_transaction().unwrap();
    assert!(!second.path_index_data_is_current("late", "[]").unwrap());
    assert_eq!(right.total_field_length("body").unwrap(), 6);
}
