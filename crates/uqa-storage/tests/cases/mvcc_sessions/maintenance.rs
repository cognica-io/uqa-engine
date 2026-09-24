//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated maintenance counters share transaction, refresh, savepoint and publication boundaries.

use super::*;
use uqa_storage::{statistics_maintenance::StatisticsMaintenance, CatalogFacade, KeyValueCatalog};

const TABLE: &str = "public.t";
const OBJECT: [u8; 16] = [1; 16];

fn catalog(persistence: &Arc<Persistence>) -> (Arc<VersionedKeyValueStore>, KeyValueCatalog) {
    let store = Arc::new(persistence.session(1 << 20));
    let catalog = KeyValueCatalog::new(store.clone());
    (store, catalog)
}

fn changes(catalog: &KeyValueCatalog, count: u64) {
    let mut state = StatisticsMaintenance::load_for(catalog, TABLE, OBJECT).unwrap();
    state.record_changes(OBJECT, count, Some(100), 200).unwrap();
    state.save(catalog, TABLE).unwrap();
}

fn analyze(catalog: &KeyValueCatalog, rows: u64) {
    StatisticsMaintenance::analyzed_for(catalog, TABLE, OBJECT, rows, 1).unwrap();
}

fn state(catalog: &KeyValueCatalog) -> serde_json::Value {
    serde_json::from_str(
        &catalog
            .get_metadata(&StatisticsMaintenance::key(TABLE))
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn analysis_and_row_changes_merge_in_either_commit_order() {
    for analysis_first in [false, true] {
        let persistence = Persistence::new();
        let (a, analyst) = catalog(&persistence);
        let (b, writer) = catalog(&persistence);
        analyze(&analyst, 100);
        changes(&analyst, 5);
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        analyze(&analyst, 105);
        changes(&writer, 2);
        let (first, last) = if analysis_first { (&a, &b) } else { (&b, &a) };
        first.commit_transaction().unwrap();
        last.commit_transaction().unwrap();
        let current = state(&writer);
        assert_eq!(current["changes"], 2);
        assert_eq!(current["generation"], 4);
        assert_eq!(current["analyzed_rows"], 105);
        assert!(StatisticsMaintenance::load(&writer, TABLE).unwrap().dirty());
        assert_eq!(a.retention_control().memory().used(), 0);
        assert_eq!(b.retention_control().memory().used(), 0);
    }
}

#[test]
fn command_refresh_and_savepoint_rollback_preserve_only_unpublished_changes() {
    let persistence = Persistence::new();
    let (a, left) = catalog(&persistence);
    let (_, right) = catalog(&persistence);
    analyze(&left, 100);
    changes(&left, 5);
    a.begin_transaction().unwrap();
    changes(&left, 3);
    let retained = a.record_snapshot().unwrap();
    let key = a.scan_prefix(b"m").unwrap().remove(0).0;
    analyze(&right, 105);
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(state(&left)["changes"], 3);
    changes(&right, 2);
    for _ in 0..2 {
        a.refresh_transaction_snapshot(a.retention_control().cancellation())
            .unwrap();
        assert_eq!(state(&left)["changes"], 5);
    }
    a.savepoint("before_analysis").unwrap();
    analyze(&left, 110);
    changes(&right, 4);
    a.rollback_to_savepoint("before_analysis").unwrap();
    a.refresh_transaction_snapshot(a.retention_control().cancellation())
        .unwrap();
    assert_eq!(state(&left)["changes"], 9);
    assert_eq!(state(&left)["analyzed_rows"], 105);
    a.commit_transaction().unwrap();
    assert_eq!(state(&right)["changes"], 9);
    let control = a.retention_control();
    let old: serde_json::Value = {
        let record = retained.get(&key, &control).unwrap().unwrap();
        serde_json::from_slice(record.value().unwrap()).unwrap()
    };
    assert_eq!(old["changes"], 8);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn canonical_writes_and_requirements_are_never_weakened_by_maintenance_merges() {
    for requirement in [false, true] {
        let persistence = Persistence::new();
        let (a, left) = catalog(&persistence);
        let (_, right) = catalog(&persistence);
        analyze(&left, 100);
        a.begin_transaction().unwrap();
        let key = a.scan_prefix(b"m").unwrap().remove(0).0;
        if requirement {
            a.with_mutation(&mut |_, batch| batch.require_unchanged(&key))
                .unwrap();
        } else {
            left.set_metadata(
                &StatisticsMaintenance::key(TABLE),
                &state(&left).to_string(),
            )
            .unwrap();
        }
        changes(&left, 3);
        changes(&right, 2);
        assert!(a.commit_transaction().is_err());
        assert_eq!(state(&right)["changes"], 2);
        a.rollback_transaction().unwrap();
    }
}

#[test]
fn removed_or_replaced_relations_reject_old_maintenance() {
    for replace in [false, true] {
        let persistence = Persistence::new();
        let (a, left) = catalog(&persistence);
        let (b, right) = catalog(&persistence);
        analyze(&left, 100);
        a.begin_transaction().unwrap();
        changes(&left, 3);
        let key = a.scan_prefix(b"m").unwrap().remove(0).0;
        if replace {
            StatisticsMaintenance::analyzed_for(&right, TABLE, [2; 16], 0, 1).unwrap();
        } else {
            b.delete(&key).unwrap();
        }
        let expected = b.get(&key).unwrap();
        assert!(a.commit_transaction().is_err());
        assert_eq!(b.get(&key).unwrap(), expected);
        a.rollback_transaction().unwrap();
    }
}

#[test]
fn a_later_tombstone_rejects_an_older_first_maintenance_record() {
    let persistence = Persistence::new();
    let (a, left) = catalog(&persistence);
    let (b, right) = catalog(&persistence);
    a.begin_transaction().unwrap();
    changes(&left, 3);
    analyze(&right, 100);
    let key = b.scan_prefix(b"m").unwrap().remove(0).0;
    b.delete(&key).unwrap();
    assert!(a.commit_transaction().is_err());
    assert!(b.get(&key).unwrap().is_none());
    a.rollback_transaction().unwrap();
}

#[test]
fn rejected_publication_preserves_the_evaluated_counter_delta() {
    for fault in [CommitFault::ConcurrentCommit, CommitFault::LoseReply] {
        let persistence = Persistence::new();
        let (a, left) = catalog(&persistence);
        let (_, right) = catalog(&persistence);
        analyze(&left, 100);
        a.begin_transaction().unwrap();
        changes(&left, 3);
        changes(&right, 2);
        persistence.state.lock().commit_fault = fault;
        if fault == CommitFault::LoseReply {
            assert!(a.commit_transaction().is_err());
            assert!(a.pending_commit().is_some());
        }
        a.commit_transaction().unwrap();
        assert_eq!(state(&right)["changes"], 5);
        assert_eq!(state(&right)["generation"], 3);
    }
}

#[test]
fn malformed_typed_keys_and_exhausted_budgets_cannot_publish_partial_counters() {
    let persistence = Persistence::new();
    let (a, left) = catalog(&persistence);
    let (_, right) = catalog(&persistence);
    analyze(&left, 100);
    a.begin_transaction().unwrap();
    a.with_mutation(&mut |_, batch| batch.replace_statistics_maintenance(b"unrelated", b"{}"))
        .unwrap();
    assert!(a.commit_transaction().is_err());
    a.rollback_transaction().unwrap();
    assert!(a.get(b"unrelated").unwrap().is_none());
    a.begin_transaction().unwrap();
    changes(&left, 3);
    changes(&right, 2);
    let control = a.retention_control();
    let reservation = control
        .memory()
        .reserve((1 << 20) - control.memory().used())
        .unwrap();
    assert!(a.commit_transaction().is_err());
    assert_eq!(state(&right)["changes"], 2);
    drop(reservation);
    a.commit_transaction().unwrap();
    assert_eq!(state(&right)["changes"], 5);
}

#[test]
fn maintenance_and_graph_occurrence_vector_effects_share_refresh_and_publication() {
    use uqa_storage::{InvertedIndex, KeyValueInvertedIndex};

    for hnsw in [false, true] {
        for refresh in [false, true] {
            let persistence = Persistence::new();
            let (a, b, mut vector_left, mut vector_right) =
                super::vector_merging::fixture(&persistence, hnsw);
            let left = KeyValueCatalog::new(a.clone());
            let right = KeyValueCatalog::new(b.clone());
            let mut text_left =
                KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
            let mut text_right =
                KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
            let fields = |text: &str| BTreeMap::from([("body".into(), text.into())]);
            text_left.add_document(1, fields("alpha")).unwrap();
            analyze(&left, 100);
            left.save_named_graph("g").unwrap();
            left.save_vertex(1, "node", "{}").unwrap();
            left.save_graph_membership("vertex", 1, "g").unwrap();
            a.begin_transaction().unwrap();
            text_left.add_document(2, fields("alpha alpha")).unwrap();
            vector_left.add(11, vec![0.5, 0.5]).unwrap();
            left.save_vertex(1, "node", "{\"changed\":true}").unwrap();
            changes(&left, 3);
            text_right
                .add_document(3, fields("alpha alpha alpha"))
                .unwrap();
            vector_right.add(12, vec![0.0, 1.0]).unwrap();
            changes(&right, 2);
            if refresh {
                a.refresh_transaction_snapshot(a.retention_control().cancellation())
                    .unwrap();
                assert_eq!(state(&left)["changes"], 5);
                vector_right.add(13, vec![0.25, 0.75]).unwrap();
                changes(&right, 4);
            }
            right.save_path_index("late", "[]").unwrap();
            right.finish_path_index_data("late", "g", "[]").unwrap();
            a.commit_transaction().unwrap();
            assert_eq!(state(&right)["changes"], if refresh { 9 } else { 5 });
            assert_eq!(vector_right.count().unwrap(), if refresh { 4 } else { 3 });
            assert_eq!(text_right.doc_count().unwrap(), 3);
            assert_eq!(text_right.total_field_length("body").unwrap(), 6);
            assert!(!right.path_index_data_is_current("late", "[]").unwrap());
            assert_eq!(
                right.graph_vertex(1).unwrap().unwrap().properties_json,
                "{\"changed\":true}"
            );
            // The live vector handle owns its last decoded physical generation and allowance.
            drop(vector_left);
            assert_eq!(a.retention_control().memory().used(), 0);
        }
    }
}
