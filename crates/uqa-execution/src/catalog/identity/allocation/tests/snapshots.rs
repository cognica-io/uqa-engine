//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog allocation reuse keeps private changes and post-wait collisions visible.

use super::*;

#[test]
fn catalog_allocations_capture_once_per_visible_revision() {
    let session = Session::new();
    session.continue_counter_at(50_000);
    let mut allocator = session.allocator();
    for (class, oid) in [
        (CatalogOidClass::Relation, 50_000),
        (CatalogOidClass::Type, 50_001),
        (CatalogOidClass::Constraint, 50_002),
    ] {
        assert_eq!(
            allocator.allocate_catalog_oid(class, &[7; 16]).unwrap(),
            oid
        );
    }
    assert_eq!(session.captures.load(Ordering::Relaxed), 1);

    // Private definitions appear before another address is allocated.
    *session.current.write() = occupied(50_003);
    session.revision.fetch_add(1, Ordering::Release);
    assert_eq!(
        allocator
            .allocate_catalog_oid(CatalogOidClass::Constraint, &[7; 16])
            .unwrap(),
        50_004
    );
    assert_eq!(session.captures.load(Ordering::Relaxed), 2);

    // A conflicting commit is first visible after the candidate's lock is acquired.
    *session.refresh.lock() = Some(domain_occupied(50_005));
    assert_eq!(
        allocator
            .allocate_catalog_oid(CatalogOidClass::Constraint, &[7; 16])
            .unwrap(),
        50_006
    );
    assert_eq!(session.captures.load(Ordering::Relaxed), 3);
    assert!(session.available(50_005));

    // Rollback removes the private owner; supplied identities see the restored view too.
    *session.current.write() = crate::catalog::test_support::empty_catalog();
    session.revision.fetch_add(1, Ordering::Release);
    allocator
        .include_catalog_identity(
            &uqa_core::RelationIdentity::new("public", "fresh"),
            CatalogOidClass::Constraint,
            ConstraintCatalogIdentity {
                object_id: [8; 16],
                oid: 50_003,
            },
        )
        .unwrap();
    assert_eq!(session.captures.load(Ordering::Relaxed), 4);
    assert!(!session.available(50_003));
}

#[test]
fn catalog_allocation_without_a_revision_keeps_both_collision_checks() {
    let mut session = Session::new();
    session.versioned = false;
    session.continue_counter_at(50_000);
    let mut allocator = session.allocator();
    allocator
        .allocate_catalog_oid(CatalogOidClass::Constraint, &[7; 16])
        .unwrap();
    allocator
        .allocate_catalog_oid_matching(CatalogOidClass::EnumLabel, |oid| oid % 2 == 0)
        .unwrap();
    assert_eq!(session.captures.load(Ordering::Relaxed), 4);
}

#[test]
fn catalog_allocation_rejects_a_view_invalidated_during_capture() {
    let session = Session::new();
    session.continue_counter_at(50_000);
    *session.capture_refresh.lock() = Some(occupied(50_000));
    assert_eq!(
        session
            .allocator()
            .allocate_catalog_oid(CatalogOidClass::Constraint, &[7; 16])
            .unwrap(),
        50_001
    );
    assert_eq!(session.captures.load(Ordering::Relaxed), 2);
    assert!(session.available(50_000));
}
