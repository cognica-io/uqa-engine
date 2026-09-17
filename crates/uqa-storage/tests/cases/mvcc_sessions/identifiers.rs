//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier allocation belongs to the durable owner and is never undone with private rows.

use super::*;

fn one() -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum: 1,
        maximum: u64::MAX,
        count: std::num::NonZeroU64::new(1).unwrap(),
    }
}

#[test]
fn durable_identifier_reservations_share_the_physical_conformance_contract() {
    let persistence = Persistence::new();
    verify_identifier_allocations(
        &*persistence,
        &*persistence,
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap();
}

#[test]
fn identifier_allocations_do_not_publish_private_records_and_survive_undo() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.begin_transaction().unwrap();
    a.put(b"private", b"row").unwrap();
    a.savepoint("before-reservation").unwrap();
    assert_eq!(
        a.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(1..=1)
    );
    assert_eq!(
        b.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(2..=2)
    );
    assert!(b.get(b"private").unwrap().is_none());
    assert_eq!(persistence.state.lock().next, 0);
    a.rollback_to_savepoint("before-reservation").unwrap();
    assert_eq!(
        a.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(3..=3)
    );
    a.rollback_transaction().unwrap();
    assert_eq!(
        a.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(4..=4)
    );
    assert!(!a.in_transaction());
    assert!(a.get(b"private").unwrap().is_none());
}

#[test]
fn identifier_allocations_reject_read_only_sessions_and_sealed_attempts() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.begin_read_transaction().unwrap();
    assert!(a.allocate_identifiers(b"entities", one()).is_err());
    a.rollback_transaction().unwrap();
    a.begin_transaction().unwrap();
    a.put(b"row", b"private").unwrap();
    persistence.state.lock().commit_fault = CommitFault::Reject;
    assert!(a.commit_transaction().is_err());
    let error = a.allocate_identifiers(b"entities", one()).unwrap_err();
    assert!(error.to_string().contains("sealed"), "{error}");
    assert_eq!(
        b.allocate_identifiers(b"entities", one()).unwrap().range(),
        Some(1..=1)
    );
    persistence.state.lock().commit_fault = CommitFault::None;
    a.commit_transaction().unwrap();
}
