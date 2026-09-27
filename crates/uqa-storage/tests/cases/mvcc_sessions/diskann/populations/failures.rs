//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_population_pending_receipts_resolve_one_evaluated_publication() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let a = seed(&persistence);
        let built = Built::capture(&a);
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(built.publish(&a, None).is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        a.commit_transaction().unwrap();
        assert_eq!(built.counts(&*a), (3, 0));
        let state = persistence.state.lock();
        assert_eq!(state.attempts[before], *state.attempts.last().unwrap());
        assert_eq!(
            state.attempts.len() - before,
            if fault == CommitFault::LoseBeforeCommit {
                2
            } else {
                1
            }
        );
        drop(state);
        replace(&a, 2, 3).unwrap();
        assert_eq!(built.counts(&*a), (4, 3));
    }
}

#[test]
fn diskann_population_missing_or_wrong_document_witness_rejects_the_whole_replacement() {
    for wrong_document in [false, true] {
        let persistence = Persistence::new();
        let a = seed(&persistence);
        let built = Built::capture(&a);
        built.publish(&a, None).unwrap();
        let key = Layout
            .witness_key(&built.header, 1, &built.control)
            .unwrap();
        if wrong_document {
            let other = Layout
                .witness_key(&built.header, 2, &built.control)
                .unwrap();
            a.put(&key, &a.get(&other).unwrap().unwrap()).unwrap();
        } else {
            a.delete(&key).unwrap();
        }
        let before = a.scan_prefix(&field()).unwrap();
        a.begin_transaction().unwrap();
        assert!(replace(&a, 1, 4).is_err());
        assert_eq!(a.scan_prefix(&field()).unwrap(), before);
        assert_eq!(built.counts(&*a), (3, 0));
        a.commit_transaction().unwrap();
        assert_eq!(a.scan_prefix(&field()).unwrap(), before);
        assert_eq!(built.counts(&*a), (3, 0));
    }
}

#[test]
fn diskann_population_invalid_complete_tensor_rolls_back_origin_and_values_together() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let built = Built::capture(&a);
    built.publish(&a, None).unwrap();
    let before = a.scan_prefix(&field()).unwrap();
    a.begin_transaction().unwrap();
    let result = a.with_versioned_mutation(&mut |mutation, _, batch| {
        let mut prefix = field();
        prefix.extend_from_slice(&1_u64.to_be_bytes());
        batch.delete_prefix(&prefix)?;
        let mut key = b"\0uqa-diskann-canonical-v1\0".to_vec();
        key.extend_from_slice(&prefix);
        let version = DiskANNVectorVersion::new(mutation.transaction(), mutation.revision())?;
        let origin = DiskANNCanonicalOrigin::new(version, 2, 4)?;
        batch.replace_diskann_origin(&key, &origin.encode())
    });
    assert!(result.is_err());
    assert_eq!(a.scan_prefix(&field()).unwrap(), before);
    assert_eq!(built.counts(&*a), (3, 0));
    replace(&a, 1, 2).unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(built.counts(&*a), (4, 2));
}

#[test]
fn diskann_population_cancelled_build_reader_rejects_even_an_empty_census() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let erased: Arc<dyn KeyValueStore> = a.clone();
    setup(&erased);
    let built = Built::capture(&a);
    let template =
        DiskANNPopulationState::from_counts(built.generation, 2, DiskANNCanonicalCounts::default())
            .unwrap()
            .encode();
    built.control.cancellation().cancel();
    a.begin_transaction().unwrap();
    let error = a
        .with_mutation(&mut |_, batch| {
            batch.put(b"publication-companion", b"must roll back")?;
            batch.publish_diskann_population(&built.header, &template, built.origins.clone())
        })
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert!(a.get(b"publication-companion").unwrap().is_none());
    assert!(a.get(&built.header).unwrap().is_none());
    a.put(b"later", b"valid").unwrap();
    a.commit_transaction().unwrap();
    assert!(a.get(&built.header).unwrap().is_none());
}

#[test]
fn diskann_population_rejected_memory_admission_preserves_the_preceding_private_view() {
    let persistence = Persistence::new();
    let a = seed(&persistence);
    let built = Built::capture(&a);
    built.publish(&a, None).unwrap();
    a.begin_transaction().unwrap();
    let control = a.retention_control();
    let mut denied = None;
    let error = a
        .with_mutation(&mut |_, batch| {
            batch.put(b"publication-companion", b"must roll back")?;
            batch.retire_diskann_population(&built.header)?;
            denied = Some(
                control
                    .memory()
                    .reserve(control.memory().limit() - control.memory().used())?,
            );
            Ok(())
        })
        .unwrap_err();
    drop(denied);
    assert!(matches!(error, StorageBackendError::Memory(_)));
    assert!(a.get(b"publication-companion").unwrap().is_none());
    assert_eq!(built.counts(&*a), (3, 0));
    replace(&a, 2, 3).unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(built.counts(&*a), (4, 3));
}
