//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent documents share one IVF generation without replaying caller evaluation.

use super::*;
use uqa_storage::{key_value::KeyValueIVFIndex, IVFIndexParams, VectorIndex};

#[test]
fn independent_ivf_writers_preserve_both_documents_when_training_changes_shared_state() {
    for seed in [0, 2, 8] {
        let persistence = Persistence::new();
        let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
        let b = a.open_session().unwrap();
        let params = IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 3,
        };
        let mut left =
            KeyValueIVFIndex::create(a.clone(), "vectors", "embedding", 2, params).unwrap();
        for doc in 1..=seed {
            left.add(doc, vec![1.0, doc as f32]).unwrap();
        }
        left.initialize().unwrap();
        let mut right =
            KeyValueIVFIndex::restore(b.clone(), "vectors", "embedding", 2, params).unwrap();
        let baseline = left.snapshot().unwrap();
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        left.add_many(11, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        right.add(12, vec![0.5, 0.5]).unwrap();
        let private = left.snapshot().unwrap();
        b.commit_transaction().unwrap();
        a.commit_transaction().unwrap();
        assert_eq!(left.count().unwrap(), seed as usize + 3);
        assert_eq!(right.count().unwrap(), seed as usize + 3);
        assert_eq!(
            left.search_knn(&[1.0, 0.0], 100).unwrap().len(),
            seed as usize + 2
        );
        assert_eq!(baseline.count().unwrap(), seed as usize);
        assert_eq!(private.count().unwrap(), seed as usize + 2);
    }
}

#[test]
fn ivf_merges_match_serial_generations_and_reject_document_and_definition_conflicts() {
    use uqa_storage::key_value::conformance::*;
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    let b = a.open_session().unwrap();
    verify_ivf_document_merges(&a, &b).unwrap();
    verify_ivf_merge_reopen(&b).unwrap();
    verify_ivf_merge_conflicts(&a, &b).unwrap();
}

fn fixture(
    persistence: &Arc<Persistence>,
) -> (
    Arc<VersionedKeyValueStore>,
    Arc<VersionedKeyValueStore>,
    KeyValueIVFIndex,
    KeyValueIVFIndex,
) {
    let a = Arc::new(persistence.session(1 << 20));
    let b = Arc::new(persistence.session(1 << 20));
    let params = IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 3,
    };
    let mut left = KeyValueIVFIndex::create(a.clone(), "vectors", "embedding", 2, params).unwrap();
    left.add(1, vec![1.0, 0.0]).unwrap();
    left.initialize().unwrap();
    let right = KeyValueIVFIndex::restore(b.clone(), "vectors", "embedding", 2, params).unwrap();
    (a, b, left, right)
}

#[test]
fn ivf_repreparation_and_receipt_recovery_preserve_the_sealed_fingerprint() {
    for fault in [
        CommitFault::ConcurrentCommit,
        CommitFault::LoseBeforeCommit,
        CommitFault::LoseReply,
    ] {
        let persistence = Persistence::new();
        let (a, _, mut left, mut right) = fixture(&persistence);
        a.begin_transaction().unwrap();
        left.add_many(11, vec![vec![0.5, 0.5], vec![0.25, 0.75]])
            .unwrap();
        right.add(12, vec![0.0, 1.0]).unwrap();
        let start = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        if fault != CommitFault::ConcurrentCommit {
            assert!(a.commit_transaction().is_err());
            assert!(a.pending_commit().is_some());
            right.add(13, vec![0.75, 0.25]).unwrap();
        }
        a.commit_transaction().unwrap();
        assert_eq!(
            right.count().unwrap(),
            if fault == CommitFault::ConcurrentCommit {
                4
            } else {
                5
            }
        );
        let state = persistence.state.lock();
        let expected = state.attempts[start];
        let retry = match fault {
            CommitFault::LoseReply => state.attempts[start], // Resolves the receipt without another prepared write.
            _ => *state.attempts.last().unwrap(),
        };
        assert_eq!(expected, retry);
        assert_eq!(
            state.attempts.len() - start,
            if fault == CommitFault::LoseBeforeCommit {
                4 // The retained candidate first fails snapshot admission after the intervening writer.
            } else {
                2
            }
        );
        if fault == CommitFault::LoseBeforeCommit {
            assert_eq!(state.attempts[start + 2], expected);
        }
        assert_eq!(a.retention_control().memory().used(), 0);
    }
}

#[test]
fn canonical_drift_cannot_be_hidden_by_an_ivf_rebase() {
    for extra_document in [11, 98] {
        let persistence = Persistence::new();
        let (a, b, mut left, mut right) = fixture(&persistence);
        a.begin_transaction().unwrap();
        left.add(11, vec![0.5, 0.5]).unwrap();
        let mut raw = uqa_storage::KeyValueVectorIndex::new(a.clone(), "vectors", "embedding", 2);
        raw.add(extra_document, vec![0.75, 0.25]).unwrap();
        right.add(12, vec![0.0, 1.0]).unwrap();
        let before = b.scan_prefix(b"").unwrap();
        let error = a.commit_transaction().unwrap_err();
        assert!(error.to_string().contains("IVF inputs disagree"), "{error}");
        a.rollback_transaction().unwrap();
        assert_eq!(before, b.scan_prefix(b"").unwrap());
        assert_eq!(right.count().unwrap(), 2);
    }
}

#[test]
fn ivf_commit_allowance_failure_preserves_the_inputs_for_retry() {
    let persistence = Persistence::new();
    let (a, _, mut left, mut right) = fixture(&persistence);
    a.begin_transaction().unwrap();
    left.add(11, vec![0.5, 0.5]).unwrap();
    right.add(12, vec![0.0, 1.0]).unwrap();
    let control = a.retention_control();
    let hold = control
        .memory()
        .reserve((1 << 20) - control.memory().used())
        .unwrap();
    assert!(a.commit_transaction().is_err());
    assert_eq!(right.count().unwrap(), 2);
    drop(hold);
    a.commit_transaction().unwrap();
    assert_eq!(right.count().unwrap(), 3);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ivf_occurrence_and_graph_changes_share_one_commit_snapshot() {
    use uqa_storage::{CatalogFacade, InvertedIndex, KeyValueCatalog, KeyValueInvertedIndex};
    let persistence = Persistence::new();
    let (a, b, mut left, mut right) = fixture(&persistence);
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b.clone());
    first.save_named_graph("g").unwrap();
    first.save_vertex(1, "node", "{}").unwrap();
    first.save_graph_membership("vertex", 1, "g").unwrap();
    let mut text_a =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::whitespace_analyzer());
    let mut text_b = KeyValueInvertedIndex::new(b, "docs", uqa_analysis::whitespace_analyzer());
    a.begin_transaction().unwrap();
    left.add(11, vec![0.5, 0.5]).unwrap();
    text_a
        .add_document(11, BTreeMap::from([("body".into(), "alpha alpha".into())]))
        .unwrap();
    first.save_vertex(1, "node", "{\"changed\":1}").unwrap();
    right.add(12, vec![0.0, 1.0]).unwrap();
    text_b
        .add_document(
            12,
            BTreeMap::from([("body".into(), "alpha alpha alpha".into())]),
        )
        .unwrap();
    second.save_path_index("late", "[]").unwrap();
    second.finish_path_index_data("late", "g", "[]").unwrap();
    persistence.state.lock().commit_fault = CommitFault::ConcurrentCommit;
    a.commit_transaction().unwrap();
    assert_eq!(right.count().unwrap(), 3);
    assert_eq!(text_b.doc_count().unwrap(), 2);
    assert_eq!(text_b.total_field_length("body").unwrap(), 5);
    assert!(!second.path_index_data_is_current("late", "[]").unwrap());
}

#[test]
fn an_ivf_rebase_cannot_overwrite_unjournaled_derived_changes() {
    let persistence = Persistence::new();
    let (a, b, mut left, mut right) = fixture(&persistence);
    left.add(2, vec![0.0, 1.0]).unwrap();
    left.add(3, vec![0.25, 0.75]).unwrap();
    a.begin_transaction().unwrap();
    left.add(11, vec![0.5, 0.5]).unwrap();
    // The fixture has one field; its physical centroid rows use the common i namespace.
    let (key, mut value) = a.scan_prefix(b"i").unwrap().remove(0);
    value[..4].copy_from_slice(&0.125_f32.to_le_bytes());
    a.put(&key, &value).unwrap();
    right.add(12, vec![0.0, 1.0]).unwrap();
    let before = b.scan_prefix(b"").unwrap();
    let error = a.commit_transaction().unwrap_err();
    assert!(
        error.to_string().contains("unjournaled derived records"),
        "{error}"
    );
    a.rollback_transaction().unwrap();
    assert_eq!(before, b.scan_prefix(b"").unwrap());
}
