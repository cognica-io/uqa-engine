//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition requirements and data markers preserve private undo and sealed commit semantics.

use super::*;

#[test]
fn revision_guards_allow_independent_writers_and_reject_definition_races() {
    let persistence = Persistence::new();
    verify_revision_guards(&persistence.session(1 << 20), &persistence.session(1 << 20)).unwrap();
}

#[test]
fn revision_requirements_detect_absent_and_recreated_records_without_reading_their_values() {
    let large_definition = vec![1; 1 << 18];
    for initial in [None, Some(&b"initial"[..])] {
        let persistence = Persistence::new();
        let a = persistence.session(16 * 1024);
        let b = persistence.session(1 << 20);
        if let Some(value) = initial {
            b.put(b"definition", value).unwrap();
        }
        a.begin_transaction().unwrap();
        let mut batch = a.batch();
        batch.require_unchanged(b"definition").unwrap();
        batch.put(b"row", b"private").unwrap();
        batch.commit().unwrap();
        b.put(b"definition", &large_definition).unwrap();
        b.delete(b"definition").unwrap();
        if let Some(value) = initial {
            b.put(b"definition", value).unwrap();
        }
        assert!(a
            .commit_transaction()
            .unwrap_err()
            .to_string()
            .contains("dependency"));
        assert!(b.get(b"row").unwrap().is_none());
        a.rollback_transaction().unwrap();
    }
}

#[test]
fn discarded_requirements_do_not_escape_failed_or_unwound_evaluation() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.put(b"definition", b"original").unwrap();
    a.begin_transaction().unwrap();
    assert!(a
        .with_mutation(&mut |_, batch| {
            batch.require_unchanged(b"definition")?;
            Err(StorageBackendError::Other("evaluation failed".into()))
        })
        .is_err());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        a.with_mutation(&mut |_, batch| {
            batch.require_unchanged(b"definition")?;
            panic!("evaluation unwound");
        })
    }))
    .is_err());
    b.put(b"definition", b"new").unwrap();
    a.put(b"unrelated", b"kept").unwrap();
    a.commit_transaction().unwrap();
}

#[test]
fn marker_preparation_retries_without_replaying_evaluation_or_dropping_requirements() {
    for definition_changes in [false, true] {
        let persistence = Persistence::new();
        let a = persistence.session(1 << 20);
        let mut calls = 0;
        persistence.state.lock().commit_fault = CommitFault::ConcurrentCommit;
        let result = a.with_mutation(&mut |_, batch| {
            calls += 1;
            batch.require_unchanged(if definition_changes {
                b"concurrent unrelated record"
            } else {
                b"stable definition"
            })?;
            batch.touch_marker(b"data marker", b"immutable")?;
            batch.put(b"row", b"value")
        });
        assert_eq!(calls, 1);
        if definition_changes {
            assert!(result.unwrap_err().to_string().contains("dependency"));
            a.rollback_transaction().unwrap();
            assert!(a.get(b"row").unwrap().is_none());
        } else {
            result.unwrap();
            {
                let state = persistence.state.lock();
                assert_eq!(state.attempts.len(), 2);
                assert_eq!(state.attempts[0], state.attempts[1]);
            }
            assert_eq!(a.get(b"row").unwrap().as_deref(), Some(&b"value"[..]));
        }
    }
}

#[test]
fn later_marker_touches_do_not_weaken_an_explicit_fence() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.begin_transaction().unwrap();
    let mut structural = a.batch();
    structural.fence_record(b"marker").unwrap();
    structural.touch_marker(b"marker", b"immutable").unwrap();
    structural.commit().unwrap();
    let mut data = b.batch();
    data.touch_marker(b"marker", b"immutable").unwrap();
    data.commit().unwrap();
    assert!(a.commit_transaction().is_err());
    a.rollback_transaction().unwrap();
    let mut wrong = a.batch();
    wrong.touch_marker(b"marker", b"different payload").unwrap();
    assert!(wrong.commit().is_err());
    a.rollback_transaction().unwrap();
    assert_eq!(
        b.get(b"marker").unwrap().as_deref(),
        Some(&b"immutable"[..])
    );
}

#[test]
fn requirements_and_markers_survive_combined_graph_vector_and_occurrence_resolution() {
    use uqa_storage::key_value::KeyValueHNSWIndex;
    use uqa_storage::{
        CatalogFacade, InvertedIndex, KeyValueCatalog, KeyValueInvertedIndex, VectorIndex,
    };
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 24));
    let b = a.open_session().unwrap();
    let catalog = KeyValueCatalog::new(a.clone());
    catalog.save_named_graph("g").unwrap();
    catalog.save_vertex(1, "node", "{}").unwrap();
    catalog.save_graph_membership("vertex", 1, "g").unwrap();
    let mut vectors = KeyValueHNSWIndex::create(
        a.clone(),
        "docs",
        "v",
        2,
        uqa_storage::HNSWIndexParams::default(),
    )
    .unwrap();
    vectors.initialize().unwrap();
    let mut occurrences =
        KeyValueInvertedIndex::new(a.clone(), "docs", uqa_analysis::Analyzer::default());
    a.put(b"definition", b"v1").unwrap();
    a.begin_transaction().unwrap();
    let mut batch = a.batch();
    batch.require_unchanged(b"definition").unwrap();
    batch.touch_marker(b"data marker", b"immutable").unwrap();
    batch.commit().unwrap();
    catalog
        .save_vertex(1, "node", "{\"changed\":true}")
        .unwrap();
    vectors.add(1, vec![1.0, 0.0]).unwrap();
    occurrences
        .add_document(1, BTreeMap::from([("body".into(), "alpha beta".into())]))
        .unwrap();
    let mut concurrent = b.batch();
    concurrent
        .touch_marker(b"data marker", b"immutable")
        .unwrap();
    concurrent.commit().unwrap();
    a.commit_transaction().unwrap();
    let mut expected = vec![b"definition".to_vec()];
    for (tag, name) in [
        (b'm', "graph_identifier_generation"),
        (b'g', "g"),
        (b'm', "graph_label_registry::g"),
    ] {
        let mut key = vec![tag];
        key.extend_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
        key.extend_from_slice(name.as_bytes());
        expected.push(key);
    }
    assert_eq!(persistence.state.lock().required_keys, expected);
    assert_eq!(vectors.count().unwrap(), 1);
    assert_eq!(occurrences.doc_count().unwrap(), 1);
    assert_eq!(
        catalog.graph_vertex(1).unwrap().unwrap().properties_json,
        "{\"changed\":true}"
    );
}

#[test]
fn requirement_memory_is_bounded_and_read_only_sessions_cannot_stage_guards() {
    let persistence = Persistence::new();
    let a = persistence.session(16 * 1024);
    a.begin_read_transaction().unwrap();
    let mut read_only = a.batch();
    read_only.require_unchanged(b"definition").unwrap();
    assert!(read_only.commit().is_err());
    a.rollback_transaction().unwrap();
    a.begin_transaction().unwrap();
    a.put(b"prior", b"kept").unwrap();
    let mut failed = false;
    for index in 0_u64..1000 {
        let mut batch = a.batch();
        let result = batch
            .require_unchanged(&index.to_be_bytes())
            .and_then(|()| batch.commit());
        match result {
            Ok(()) => {}
            Err(StorageBackendError::Memory(_)) => {
                failed = true;
                break;
            }
            Err(error) => panic!("unexpected guard error: {error}"),
        }
    }
    assert!(failed);
    assert_eq!(a.get(b"prior").unwrap().as_deref(), Some(&b"kept"[..]));
    a.rollback_transaction().unwrap();
    assert_eq!(a.retention_control().memory().used(), 0);
}

#[test]
fn requirements_distinguish_receipts_and_confirmed_retries_skip_later_changes() {
    let fingerprint = |requirement: Option<&[u8]>| {
        let persistence = Persistence::new();
        let a = persistence.session(1 << 20);
        a.with_mutation(&mut |_, batch| {
            if let Some(key) = requirement {
                batch.require_unchanged(key)?;
            }
            batch.put(b"row", b"value")
        })
        .unwrap();
        let fingerprint = persistence.state.lock().attempts[0];
        fingerprint
    };
    let first = fingerprint(Some(b"first definition"));
    assert_ne!(first, fingerprint(None));
    assert_ne!(first, fingerprint(Some(b"second definition")));
    assert_eq!(first, fingerprint(Some(b"first definition")));

    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.put(b"definition", b"initial").unwrap();
    persistence.state.lock().commit_fault = CommitFault::LoseReply;
    assert!(a
        .with_mutation(&mut |_, batch| {
            batch.require_unchanged(b"definition")?;
            batch.touch_marker(b"data marker", b"immutable")?;
            batch.put(b"row", b"value")
        })
        .is_err());
    b.put(b"definition", b"later change").unwrap();
    let sequence = persistence.store.snapshot().unwrap().sequence();
    a.commit_transaction().unwrap();
    assert_eq!(persistence.store.snapshot().unwrap().sequence(), sequence);
    assert_eq!(b.get(b"row").unwrap().as_deref(), Some(&b"value"[..]));
}

#[test]
fn requirement_only_commit_does_not_rewrite_definitions_and_undo_keeps_prior_reads() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.put(b"definition", b"initial").unwrap();
    let sequence = persistence.store.snapshot().unwrap().sequence();
    let mut batch = a.batch();
    batch.require_unchanged(b"definition").unwrap();
    batch.commit().unwrap();
    assert_eq!(persistence.store.snapshot().unwrap().sequence(), sequence);

    a.begin_transaction().unwrap();
    let mut batch = a.batch();
    batch.require_unchanged(b"definition").unwrap();
    batch.commit().unwrap();
    a.savepoint("prior-read").unwrap();
    let mut batch = a.batch();
    batch.require_unchanged(b"definition").unwrap();
    batch.require_unchanged(b"discarded definition").unwrap();
    batch.commit().unwrap();
    a.rollback_to_savepoint("prior-read").unwrap();
    b.put(b"definition", b"changed").unwrap();
    assert!(a.commit_transaction().is_err());
    a.rollback_transaction().unwrap();
}
