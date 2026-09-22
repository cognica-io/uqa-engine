//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session attachment preserves committed/private boundaries and read-only ownership.

use super::*;
use uqa_storage::{KeyValueStorageBackend, PersistentStorageBackend};

#[test]
fn retained_backend_readers_share_the_original_limit_and_release_their_last_charge() {
    let persistence = Persistence::new();
    let source = Arc::new(persistence.session(1 << 20));
    let control = source.retention_control();
    let backend = KeyValueStorageBackend::new(source.clone());
    let reader = backend
        .open_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    let nested = reader
        .backend
        .open_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    let nested_control = nested.backend.retention_control().unwrap();
    assert!(nested_control.memory().shares_allowance(control.memory()));
    control.cancellation().cancel();
    nested_control.check().unwrap();
    control.cancellation().reset();
    assert!(backend
        .retention_control()
        .unwrap()
        .memory()
        .shares_allowance(control.memory()));
    let independent = backend.open_session().unwrap();
    assert!(!independent
        .backend
        .retention_control()
        .unwrap()
        .memory()
        .shares_allowance(control.memory()));
    let retained = control.memory().used();
    let reservation = nested_control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    assert!(control.memory().reserve(1).is_err());
    drop(source);
    drop(backend);
    drop(reader);
    drop(nested);
    assert!(control.memory().used() >= reservation.bytes());
    drop(reservation);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_sessions_keep_private_tombstones_and_the_original_committed_boundary() {
    let persistence = Persistence::new();
    let source = persistence.session(1 << 20);
    let peer = persistence.session(1 << 20);
    source.put(b"changed", b"committed").unwrap();
    source.put(b"deleted", b"committed").unwrap();
    source.begin_transaction().unwrap();
    source.put(b"changed", b"private").unwrap();
    source.delete(b"deleted").unwrap();
    source.put(b"new", b"private").unwrap();
    let cancellation = uqa_core::CancellationToken::new();
    let retained = source.new_retained_read_session(&cancellation).unwrap();
    let version = retained.change_version().unwrap();
    let expected = source.scan_prefix(b"").unwrap();
    peer.put(b"later", b"committed after capture").unwrap();
    source.refresh_transaction_snapshot(&cancellation).unwrap();
    source.put(b"changed", b"newer private").unwrap();
    assert_eq!(retained.scan_prefix(b"").unwrap(), expected);
    let view = retained.record_snapshot().unwrap();
    let private = view
        .get(b"new", &source.retention_control())
        .unwrap()
        .unwrap();
    assert!(private.is_private());
    assert_eq!(private.original_revision(), None);
    assert!(view
        .committed()
        .get(b"new", &source.retention_control())
        .unwrap()
        .is_none());
    drop(view);
    source.rollback_transaction().unwrap();
    drop(source);
    retained.begin_upgradeable_transaction().unwrap();
    retained.savepoint("reader").unwrap();
    retained
        .refresh_transaction_snapshot(&cancellation)
        .unwrap();
    retained.rollback_to_savepoint("reader").unwrap();
    retained.release_savepoint("reader").unwrap();
    let allocations = persistence.state.lock().next;
    retained.commit_transaction().unwrap();
    assert_eq!(persistence.state.lock().next, allocations);
    assert_eq!(retained.change_version().unwrap(), version);
    assert_eq!(retained.scan_prefix(b"").unwrap(), expected);
    let nested = retained.new_retained_read_session(&cancellation).unwrap();
    drop(retained);
    nested.begin_read_transaction().unwrap();
    nested.rollback_transaction().unwrap();
    assert_eq!(nested.scan_prefix(b"").unwrap(), expected);
    assert_eq!(peer.get(b"changed").unwrap().unwrap(), b"committed");
    assert_eq!(peer.get(b"deleted").unwrap().unwrap(), b"committed");
    assert!(peer.get(b"new").unwrap().is_none());
}

#[test]
fn retained_sessions_reject_all_publication_paths_before_evaluating_a_mutation() {
    let persistence = Persistence::new();
    let source = persistence.session(1 << 20);
    let reader = source
        .new_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    for active in [false, true] {
        if active {
            reader.begin_read_transaction().unwrap();
        }
        assert!(reader.begin_transaction().is_err());
        assert!(reader.put(b"key", b"forbidden").is_err());
        assert!(reader.delete(b"key").is_err());
        assert!(reader.delete_prefix(b"").is_err());
        assert!(reader.establish_serializable_snapshot().is_err());
        let mut called = false;
        assert!(reader
            .with_mutation(&mut |_, _| {
                called = true;
                Ok(())
            })
            .is_err());
        assert!(!called);
        let mut batch = reader.batch();
        batch.put(b"key", b"forbidden").unwrap();
        assert!(batch.commit().is_err());
        assert!(reader
            .allocate_identifiers(
                b"ids",
                IdentifierRequest::Reserve {
                    minimum: 1,
                    maximum: u64::MAX,
                    count: std::num::NonZeroU64::new(1).unwrap(),
                }
            )
            .is_err());
        if active {
            reader.rollback_transaction().unwrap();
        }
    }
    assert_eq!(persistence.state.lock().next, 0);
    assert!(persistence.state.lock().identifiers.is_empty());
    assert!(source.scan_prefix(b"").unwrap().is_empty());
}

#[test]
fn retained_backend_attachment_preserves_affinity_and_checks_cancellation_before_capture() {
    let persistence = Persistence::new();
    let source = Arc::new(persistence.session(1 << 20));
    source.put(b"key", b"original").unwrap();
    let backend = KeyValueStorageBackend::new(source.clone());
    let cancellation = uqa_core::CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        backend.open_retained_read_session(&cancellation),
        Err(StorageBackendError::Cancelled(_))
    ));
    cancellation.reset();
    let reader = backend.open_retained_read_session(&cancellation).unwrap();
    reader.validate_transaction_affinity().unwrap();
    assert_ne!(
        reader.backend.transaction_affinity(),
        backend.transaction_affinity()
    );
    let version = reader.backend.change_version().unwrap();
    source.put(b"later", b"new commit").unwrap();
    reader.backend.begin_read_transaction().unwrap();
    reader
        .backend
        .refresh_transaction_snapshot(&cancellation)
        .unwrap();
    reader.backend.rollback_transaction().unwrap();
    assert_eq!(reader.backend.change_version().unwrap(), version);
    assert_ne!(
        reader.backend.change_version().unwrap(),
        backend.change_version().unwrap()
    );
}
