//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CommitFault, Persistence};
use std::{collections::BTreeMap, sync::Arc};
use uqa_storage::{InvertedIndex, KeyValueInvertedIndex, KeyValueStore};

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

#[test]
fn occurrence_document_merges_preserve_all_conformance_invariants() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let b: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_occurrence_concurrency(&a, &b).unwrap();
    uqa_storage::key_value::conformance::verify_occurrence_accelerators(&a, &b).unwrap();
}

#[test]
fn occurrence_repreparation_and_lost_replies_keep_one_sealed_delta() {
    for fault in [CommitFault::ConcurrentCommit, CommitFault::LoseReply] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 20));
        let b = Arc::new(persistence.session(1 << 20));
        let mut left =
            KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
        let mut right = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
        left.add_document(1, fields("alpha")).unwrap();
        a.begin_transaction().unwrap();
        left.add_document(2, fields("alpha alpha")).unwrap();
        right.add_document(3, fields("alpha alpha alpha")).unwrap();
        let start = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        if fault == CommitFault::LoseReply {
            assert!(a.commit_transaction().is_err());
            assert!(a.pending_commit().is_some());
        }
        a.commit_transaction().unwrap();
        assert_eq!(right.doc_count().unwrap(), 3);
        assert_eq!(right.total_field_length("body").unwrap(), 6);
        let state = persistence.state.lock();
        let attempts = &state.attempts[start..];
        assert_eq!(
            attempts.len(),
            if fault == CommitFault::ConcurrentCommit {
                2
            } else {
                1
            }
        );
        assert!(attempts
            .iter()
            .all(|fingerprint| fingerprint == &attempts[0]));
        assert_eq!(a.retention_control().memory().used(), 0);
    }
}

#[test]
fn occurrence_commit_budget_failure_preserves_the_evaluated_batch() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 20));
    let b = Arc::new(persistence.session(1 << 20));
    let mut left =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let mut right = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    left.add_document(1, fields("alpha")).unwrap();
    a.begin_transaction().unwrap();
    left.add_document(2, fields("alpha alpha")).unwrap();
    right.add_document(3, fields("alpha alpha alpha")).unwrap();
    let control = a.retention_control();
    let occupied = control.memory().used();
    let hold = control.memory().reserve((1 << 20) - occupied).unwrap();
    assert!(a.commit_transaction().is_err());
    assert_eq!(right.doc_count().unwrap(), 2);
    drop(hold);
    a.commit_transaction().unwrap();
    assert_eq!(right.doc_count().unwrap(), 3);
    assert_eq!(right.total_field_length("body").unwrap(), 6);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn occurrence_and_graph_effects_share_one_fresh_commit_snapshot() {
    use uqa_storage::{CatalogFacade, KeyValueCatalog};
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 20));
    let b = Arc::new(persistence.session(1 << 20));
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b.clone());
    first.save_named_graph("g").unwrap();
    first.save_vertex(1, "node", "{}").unwrap();
    first.save_graph_membership("vertex", 1, "g").unwrap();
    let mut left =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let mut right = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    left.add_document(1, fields("alpha")).unwrap();
    a.begin_transaction().unwrap();
    left.add_document(2, fields("alpha alpha")).unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    right.add_document(3, fields("alpha alpha alpha")).unwrap();
    second.save_path_index("late", "[]").unwrap();
    second.finish_path_index_data("late", "g", "[]").unwrap();
    persistence.state.lock().commit_fault = CommitFault::ConcurrentCommit;
    a.commit_transaction().unwrap();
    assert_eq!(right.doc_count().unwrap(), 3);
    assert_eq!(right.total_field_length("body").unwrap(), 6);
    assert!(!second.path_index_data_is_current("late", "[]").unwrap());
    assert_eq!(
        second.graph_vertex(1).unwrap().unwrap().properties_json,
        "{\"changed\":1}"
    );
}
