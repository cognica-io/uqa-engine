//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-derived path state follows source changes without serializing independent writers.

use uqa_storage::{CatalogFacade, KeyValueCatalog};

use super::*;

fn catalogs() -> (
    Arc<dyn KeyValueStore>,
    KeyValueCatalog,
    Arc<dyn KeyValueStore>,
    KeyValueCatalog,
) {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let b = a.open_session().unwrap();
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b.clone());
    first.save_named_graph("g").unwrap();
    for id in 1..=2 {
        first.save_vertex(id, "node", "{}").unwrap();
        first.save_graph_membership("vertex", id, "g").unwrap();
    }
    (a, first, b, second)
}

fn build(catalog: &KeyValueCatalog, index: &str, graph: &str) {
    catalog.save_path_index(index, "[]").unwrap();
    catalog.finish_path_index_data(index, graph, "[]").unwrap();
    assert!(catalog.path_index_data_is_current(index, "[]").unwrap());
}

#[test]
fn path_invalidation_merges_independent_graph_writers() {
    let (a, first, b, second) = catalogs();
    build(&first, "paths", "g");
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    b.begin_transaction().unwrap();
    second.save_vertex(2, "node", "{\"changed\":2}").unwrap();
    b.commit_transaction().unwrap();
    a.commit_transaction().unwrap();
    assert!(!first.path_index_data_is_current("paths", "[]").unwrap());
    assert_eq!(
        first.graph_vertex(1).unwrap().unwrap().properties_json,
        "{\"changed\":1}"
    );
    assert_eq!(
        first.graph_vertex(2).unwrap().unwrap().properties_json,
        "{\"changed\":2}"
    );
}

#[test]
fn path_invalidation_covers_indexes_created_after_the_writer_snapshot() {
    let (a, first, _, second) = catalogs();
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    build(&second, "late", "g");
    a.commit_transaction().unwrap();
    assert!(!second.path_index_data_is_current("late", "[]").unwrap());
}

#[test]
fn path_invalidation_covers_memberships_added_after_the_writer_snapshot() {
    let (a, first, _, second) = catalogs();
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    second.save_named_graph("late").unwrap();
    second.save_graph_membership("vertex", 1, "late").unwrap();
    build(&second, "late", "late");
    a.commit_transaction().unwrap();
    assert!(!second.path_index_data_is_current("late", "[]").unwrap());
}

#[test]
fn path_build_cannot_publish_a_stale_graph_snapshot() {
    let (a, first, _, second) = catalogs();
    first.save_path_index("paths", "[]").unwrap();
    a.begin_transaction().unwrap();
    first.finish_path_index_data("paths", "g", "[]").unwrap();
    second.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    a.commit_transaction().unwrap();
    assert!(!first.path_index_data_is_current("paths", "[]").unwrap());
}

#[test]
fn derived_dependencies_retry_after_an_admitted_concurrent_commit_without_reallocating() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let catalog = KeyValueCatalog::new(a.clone());
    catalog.save_named_graph("g").unwrap();
    catalog.save_vertex(1, "node", "{}").unwrap();
    catalog.save_graph_membership("vertex", 1, "g").unwrap();
    build(&catalog, "paths", "g");
    a.begin_transaction().unwrap();
    catalog.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    let allocated = {
        let mut state = persistence.state.lock();
        state.attempts.clear();
        state.commit_fault = CommitFault::ConcurrentCommit;
        state.next
    };
    a.commit_transaction().unwrap();
    let state = persistence.state.lock();
    assert_eq!(state.next, allocated + 1);
    assert_eq!(state.attempts.len(), 2);
    assert_eq!(state.attempts[0], state.attempts[1]);
    drop(state);
    assert_eq!(
        a.get(b"concurrent unrelated record").unwrap().as_deref(),
        Some(b"committed".as_slice())
    );
    assert!(!catalog.path_index_data_is_current("paths", "[]").unwrap());
}

#[test]
fn a_lost_graph_commit_reply_resolves_before_touching_a_later_rebuild() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let b = a.open_session().unwrap();
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b);
    first.save_named_graph("g").unwrap();
    first.save_vertex(1, "node", "{}").unwrap();
    first.save_graph_membership("vertex", 1, "g").unwrap();
    build(&first, "paths", "g");
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    persistence.state.lock().commit_fault = CommitFault::LoseReply;
    assert!(matches!(
        a.commit_transaction().unwrap_err().commit_outcome(),
        Some(CommitErrorOutcome::Indeterminate(_))
    ));
    assert!(!second.path_index_data_is_current("paths", "[]").unwrap());
    second.finish_path_index_data("paths", "g", "[]").unwrap();
    a.commit_transaction().unwrap();
    assert!(first.path_index_data_is_current("paths", "[]").unwrap());
}

#[test]
fn graph_effect_order_and_savepoints_preserve_a_rebuilt_cache() {
    for rollback in [false, true] {
        let (a, first, _, second) = catalogs();
        build(&first, "paths", "g");
        first.save_named_graph("unrelated").unwrap();
        build(&first, "unrelated", "unrelated");
        a.begin_transaction().unwrap();
        first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
        first.finish_path_index_data("paths", "g", "[]").unwrap();
        a.savepoint("rebuilt").unwrap();
        first.save_vertex(2, "node", "{\"changed\":2}").unwrap();
        assert!(!first.path_index_data_is_current("paths", "[]").unwrap());
        if rollback {
            a.rollback_to_savepoint("rebuilt").unwrap();
            assert!(first.path_index_data_is_current("paths", "[]").unwrap());
        }
        second.save_vertex(3, "outside", "{}").unwrap();
        a.commit_transaction().unwrap();
        assert_eq!(
            first.path_index_data_is_current("paths", "[]").unwrap(),
            rollback
        );
        assert!(first.path_index_data_is_current("unrelated", "[]").unwrap());
    }
}

#[test]
fn graph_only_effects_are_savepoint_writes_and_have_distinct_receipts() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    a.begin_transaction().unwrap();
    a.savepoint("empty").unwrap();
    let mut batch = a.batch();
    batch
        .graph_mutation(GraphMutation::InvalidateGraph("discarded"))
        .unwrap();
    batch.commit().unwrap();
    assert!(a.transaction_has_written().unwrap());
    a.rollback_to_savepoint("empty").unwrap();
    assert!(!a.transaction_has_written().unwrap());
    a.rollback_transaction().unwrap();
    for graph in ["a", "b"] {
        let mut batch = a.batch();
        batch
            .graph_mutation(GraphMutation::InvalidateGraph(graph))
            .unwrap();
        batch.commit().unwrap();
    }
    let state = persistence.state.lock();
    assert_eq!(state.next, 2);
    assert_eq!(state.attempts.len(), 2);
    assert_ne!(state.attempts[0], state.attempts[1]);
    assert!(state
        .receipts
        .values()
        .all(|receipt| matches!(receipt, CommitStatus::Committed(_))));
}

#[test]
fn graph_cache_provider_contract_runs_against_common_records() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let b = a.open_session().unwrap();
    uqa_storage::key_value::conformance::verify_graph_cache_concurrency(a, b).unwrap();
}

#[test]
fn a_graph_writer_cannot_invalidate_a_cache_reassigned_to_another_graph() {
    let (a, first, _, second) = catalogs();
    build(&first, "paths", "g");
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    second.save_named_graph("other").unwrap();
    second
        .finish_path_index_data("paths", "other", "[]")
        .unwrap();
    a.commit_transaction().unwrap();
    assert!(second.path_index_data_is_current("paths", "[]").unwrap());
}

fn validity_key(persistence: &Persistence) -> uqa_core::memory::BudgetedVec<u8> {
    uqa_storage::key_value::KeyValueGraphRecords
        .key(
            persistence.database_id(),
            GraphRecordKey::PathValidity("paths"),
            &StorageReadControl::with_limit(1024),
        )
        .unwrap()
}

#[test]
fn cache_clear_survives_an_absent_marker_at_the_transaction_snapshot() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.open_session().unwrap();
    let key = validity_key(&persistence);
    a.begin_transaction().unwrap();
    let mut batch = a.batch();
    batch.replace_graph_cache(&key, None).unwrap();
    batch.commit().unwrap();
    b.put(&key, b"[]").unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(b.get(&key).unwrap(), None);
}

#[test]
fn canonical_graph_conflicts_do_not_publish_cache_effects() {
    let (a, first, _, second) = catalogs();
    build(&first, "paths", "g");
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"writer\":1}").unwrap();
    second.save_vertex(1, "node", "{\"writer\":2}").unwrap();
    second.finish_path_index_data("paths", "g", "[]").unwrap();
    let error = a.commit_transaction().unwrap_err();
    assert!(matches!(error, StorageBackendError::Backend { source, .. }
        if matches!(source.downcast_ref::<VersionError>(), Some(VersionError::WriteConflict { .. }))));
    a.rollback_transaction().unwrap();
    assert_eq!(
        first.graph_vertex(1).unwrap().unwrap().properties_json,
        "{\"writer\":2}"
    );
    assert!(first.path_index_data_is_current("paths", "[]").unwrap());
}

#[test]
fn raw_cache_keys_keep_canonical_write_conflicts() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.open_session().unwrap();
    let key = validity_key(&persistence);
    a.put(&key, b"[]").unwrap();
    a.begin_transaction().unwrap();
    a.delete(&key).unwrap();
    let mut batch = a.batch();
    batch
        .graph_mutation(GraphMutation::InvalidateGraph("unrelated"))
        .unwrap();
    batch.commit().unwrap();
    b.put(&key, b"newer").unwrap();
    let error = a.commit_transaction().unwrap_err();
    assert!(matches!(error, StorageBackendError::Backend { source, .. }
        if matches!(source.downcast_ref::<VersionError>(), Some(VersionError::WriteConflict { .. }))));
    a.rollback_transaction().unwrap();
    assert_eq!(b.get(&key).unwrap().as_deref(), Some(b"newer".as_slice()));
}

#[test]
fn graph_only_effects_cannot_write_in_a_read_transaction() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    a.begin_read_transaction().unwrap();
    let mut batch = a.batch();
    batch
        .graph_mutation(GraphMutation::InvalidateGraph("g"))
        .unwrap();
    assert!(batch.commit().is_err());
    assert!(!a.transaction_has_written().unwrap());
    a.commit_transaction().unwrap();
    assert_eq!(persistence.state.lock().next, 0);
}
