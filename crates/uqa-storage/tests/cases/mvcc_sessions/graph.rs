//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-derived path state follows source changes without serializing independent writers.

use uqa_storage::{CatalogFacade, KeyValueCatalog};

use super::*;

#[test]
fn graph_lifetime_dependencies_distinguish_topology_from_properties_and_follow_undo() {
    use uqa_storage::GraphEntityKind;
    let (a, first, b, second) = catalogs();
    a.begin_transaction().unwrap();
    b.begin_transaction().unwrap();
    first.save_edge(10, 1, 2, "link", "{}").unwrap();
    first.save_graph_membership("edge", 10, "g").unwrap();
    second.save_edge(11, 1, 2, "link", "{}").unwrap();
    second.save_graph_membership("edge", 11, "g").unwrap();
    b.commit_transaction().unwrap();
    a.commit_transaction().unwrap();
    for property_wins in [false, true] {
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        first.save_edge(12, 1, 2, "link", "{}").unwrap();
        first.save_graph_membership("edge", 12, "g").unwrap();
        second.save_vertex(1, "node", "{\"updated\":true}").unwrap();
        second.save_graph_membership("vertex", 1, "g").unwrap();
        let (winner, next) = if property_wins { (&b, &a) } else { (&a, &b) };
        winner.commit_transaction().unwrap();
        next.commit_transaction().unwrap();
    }
    a.begin_transaction().unwrap();
    a.savepoint("before-edge").unwrap();
    first.save_edge(13, 1, 2, "link", "{}").unwrap();
    first.save_graph_membership("edge", 13, "g").unwrap();
    a.rollback_to_savepoint("before-edge").unwrap();
    second.delete_graph_membership("vertex", 1, "g").unwrap();
    second.delete_vertex(1).unwrap();
    first.save_vertex(5, "independent", "{}").unwrap();
    a.commit_transaction().unwrap();
    assert!(first.graph_edge(13).unwrap().is_none());
    assert!(!first
        .graph_has_membership(GraphEntityKind::Vertex, 1, "g")
        .unwrap());
}

#[test]
fn graph_edge_endpoint_changes_and_new_memberships_conflict_in_both_orders() {
    for topology_wins in [false, true] {
        let (a, first, b, second) = catalogs();
        first.save_named_graph("other").unwrap();
        first.save_vertex(3, "node", "{}").unwrap();
        first.save_graph_membership("vertex", 3, "g").unwrap();
        for id in [1, 2] {
            first.save_graph_membership("vertex", id, "other").unwrap();
        }
        first.save_edge(10, 1, 2, "link", "{}").unwrap();
        first.save_graph_membership("edge", 10, "g").unwrap();
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        first.save_edge(10, 3, 2, "link", "{}").unwrap();
        second.save_graph_membership("edge", 10, "other").unwrap();
        let (winner, loser) = if topology_wins { (&a, &b) } else { (&b, &a) };
        winner.commit_transaction().unwrap();
        assert!(loser.commit_transaction().is_err());
        loser.rollback_transaction().unwrap();
        assert_eq!(
            first.graph_edge(10).unwrap().unwrap().source_id,
            if topology_wins { 3 } else { 1 }
        );
    }
}

#[test]
fn graph_endpoint_and_orphan_lifetimes_reject_both_commit_orders() {
    for membership_only in [false, true] {
        for reference_wins in [false, true] {
            let (a, first, b, second) = catalogs();
            a.begin_transaction().unwrap();
            b.begin_transaction().unwrap();
            first.save_edge(10, 1, 2, "link", "{}").unwrap();
            first.save_graph_membership("edge", 10, "g").unwrap();
            second.delete_graph_membership("vertex", 1, "g").unwrap();
            if !membership_only {
                second.delete_vertex(1).unwrap();
            }
            let (winner, loser) = if reference_wins { (&a, &b) } else { (&b, &a) };
            winner.commit_transaction().unwrap();
            assert!(
                loser.commit_transaction().is_err(),
                "membership_only={membership_only}, reference_wins={reference_wins}"
            );
            loser.rollback_transaction().unwrap();
            assert_eq!(first.graph_edge(10).unwrap().is_some(), reference_wins);
            assert_eq!(
                first
                    .graph_has_membership(uqa_storage::GraphEntityKind::Vertex, 1, "g")
                    .unwrap(),
                reference_wins
            );
        }
    }
    for reference_wins in [false, true] {
        let (a, first, b, second) = catalogs();
        first.save_vertex(3, "orphan", "{}").unwrap();
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        first.save_graph_membership("vertex", 3, "g").unwrap();
        second.purge_orphan_graph_entities().unwrap();
        let (winner, loser) = if reference_wins { (&a, &b) } else { (&b, &a) };
        winner.commit_transaction().unwrap();
        assert!(
            loser.commit_transaction().is_err(),
            "orphan reference_wins={reference_wins}"
        );
        loser.rollback_transaction().unwrap();
        assert_eq!(first.graph_vertex(3).unwrap().is_some(), reference_wins);
    }
}

#[test]
fn graph_count_and_maximum_keep_one_view_across_identity_pages() {
    use super::occurrences::InterleavedStore;
    use uqa_storage::{GraphEntityFilter, GraphEntityKind};
    for maximum in [false, true] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 22));
        let b = Arc::new(persistence.session(1 << 22));
        let second = KeyValueCatalog::new(b.clone());
        b.begin_transaction().unwrap();
        for id in 1..=257 {
            second.save_vertex(id, "item", "{}").unwrap();
        }
        b.commit_transaction().unwrap();
        let wrapper = Arc::new(InterleavedStore::new(a));
        let first = KeyValueCatalog::new(wrapper.clone());
        *wrapper.after_keys.lock() = Some(Box::new(move || {
            b.begin_transaction().unwrap();
            second.delete_vertex(257).unwrap();
            second.save_vertex(512, "item", "{}").unwrap();
            second.save_vertex(513, "item", "{}").unwrap();
            b.commit_transaction().unwrap();
        }));
        if maximum {
            assert_eq!(
                first.graph_entity_max_id(GraphEntityKind::Vertex).unwrap(),
                Some(257)
            );
            assert_eq!(
                first.graph_entity_max_id(GraphEntityKind::Vertex).unwrap(),
                Some(513)
            );
        } else {
            let filter = GraphEntityFilter::new(GraphEntityKind::Vertex, None);
            assert_eq!(first.graph_entity_count(filter).unwrap(), 257);
            assert_eq!(first.graph_entity_count(filter).unwrap(), 258);
        }
        assert!(wrapper.after_keys.lock().is_none());
    }
}

#[test]
fn graph_snapshot_keeps_entities_and_memberships_on_one_view() {
    use super::occurrences::InterleavedStore;
    use std::sync::atomic::Ordering;
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let b = Arc::new(persistence.session(1 << 22));
    let second = KeyValueCatalog::new(b);
    second.save_named_graph("g").unwrap();
    second.save_vertex(1, "before", "{}").unwrap();
    second.save_graph_membership("vertex", 1, "g").unwrap();
    let wrapper = Arc::new(InterleavedStore::new(a));
    let first = KeyValueCatalog::new(wrapper.clone());
    *wrapper.after_second_point.lock() = Some(Box::new(move || {
        second.delete_graph_membership("vertex", 1, "g").unwrap();
        second.delete_vertex(1).unwrap();
        second.save_vertex(2, "after", "{}").unwrap();
        second.save_graph_membership("vertex", 2, "g").unwrap();
    }));
    let pinned = first.load_named_graph_snapshot("g").unwrap().unwrap();
    assert!(wrapper.point_reads.load(Ordering::Relaxed) >= 2);
    assert_eq!(pinned.vertices.len(), 1);
    assert_eq!(pinned.vertices[0].vertex_id, 1);
    assert_eq!(
        first
            .load_named_graph_snapshot("g")
            .unwrap()
            .unwrap()
            .vertices[0]
            .vertex_id,
        2
    );
}

#[test]
fn graph_lookup_mutation_keeps_original_preconditions_without_replaying() {
    use super::occurrences::InterleavedStore;
    use std::sync::atomic::Ordering;
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let b = Arc::new(persistence.session(1 << 22));
    let second = KeyValueCatalog::new(b);
    second.save_vertex(1, "before", "{}").unwrap();
    let wrapper = Arc::new(InterleavedStore::new(a.clone()));
    let first = KeyValueCatalog::new(wrapper.clone());
    *wrapper.after_evaluation.lock() = Some(Box::new(move || {
        second.save_vertex(1, "winner", "{}").unwrap();
    }));
    assert!(first.save_vertex(1, "loser", "{}").is_err());
    assert_eq!(wrapper.evaluations.load(Ordering::Relaxed), 1);
    a.rollback_transaction().unwrap();
    assert_eq!(first.graph_vertex(1).unwrap().unwrap().label, "winner");
    for label in ["before", "loser"] {
        let mut filter =
            uqa_storage::GraphEntityFilter::new(uqa_storage::GraphEntityKind::Vertex, None);
        filter.label = Some(label);
        assert!(first.graph_entity_ids(filter, None, 10).unwrap().is_empty());
    }
}

#[test]
fn graph_replacement_uses_only_the_final_duplicate_entity_rows() {
    use uqa_storage::{EdgeRow, GraphEntityFilter, GraphEntityKind, GraphSnapshot, GraphVertexRow};
    let (a, first, _, _) = catalogs();
    let replacement = GraphSnapshot {
        label_registry_json: "{}".into(),
        vertices: ["discarded", "final"]
            .into_iter()
            .map(|label| GraphVertexRow {
                vertex_id: 7,
                label: label.into(),
                properties_json: "{}".into(),
            })
            .collect(),
        edges: ["discarded", "final"]
            .into_iter()
            .map(|label| EdgeRow {
                edge_id: 9,
                source_id: 7,
                target_id: 7,
                label: label.into(),
                properties_json: "{}".into(),
            })
            .collect(),
    };
    first.replace_named_graph("g", &replacement).unwrap();
    first.delete_graph_membership_for_graph("g").unwrap();
    first.delete_edge(9).unwrap();
    first.delete_vertex(7).unwrap();
    for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
        let mut filter = GraphEntityFilter::new(kind, None);
        filter.label = Some("discarded");
        assert!(first.graph_entity_ids(filter, None, 10).unwrap().is_empty());
    }
    assert!(!a.in_transaction());
}

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

#[test]
fn a_path_definition_effect_invalidates_a_later_cache_build() {
    let (a, first, _, second) = catalogs();
    a.begin_transaction().unwrap();
    let mut batch = a.batch();
    batch
        .graph_mutation(GraphMutation::InvalidatePath("paths"))
        .unwrap();
    batch.commit().unwrap();
    build(&second, "paths", "g");
    a.commit_transaction().unwrap();
    assert!(!first.path_index_data_is_current("paths", "[]").unwrap());
}

#[test]
fn an_invalidation_preview_preserves_an_earlier_canonical_write_precondition() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.open_session().unwrap();
    let key = validity_key(&persistence);
    a.put(&key, b"[]").unwrap();
    a.begin_transaction().unwrap();
    a.put(&key, b"private").unwrap();
    let mut batch = a.batch();
    batch.preview_graph_invalidation(&key, None).unwrap();
    batch
        .graph_mutation(GraphMutation::InvalidatePath("paths"))
        .unwrap();
    batch.commit().unwrap();
    b.put(&key, b"newer").unwrap();
    let error = a.commit_transaction().unwrap_err();
    assert!(
        matches!(error,StorageBackendError::Backend{source,..} if matches!(source.downcast_ref::<VersionError>(),Some(VersionError::WriteConflict{..})))
    );
    a.rollback_transaction().unwrap();
    assert_eq!(b.get(&key).unwrap().as_deref(), Some(b"newer".as_slice()));
}
