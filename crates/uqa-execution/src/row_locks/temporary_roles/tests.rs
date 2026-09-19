//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn committed_references_outlive_transactions_and_exclude_the_requesting_session() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    manager
        .prepare_temporary_roles(1, BTreeSet::from([20, 21]), &cancel)
        .unwrap()
        .commit();
    manager.release_session(1);
    assert!(manager
        .peer_temporary_role_reference(2, 20, &cancel)
        .unwrap());
    assert!(!manager
        .peer_temporary_role_reference(1, 20, &cancel)
        .unwrap());
    assert!(!manager
        .peer_temporary_role_reference(2, 22, &cancel)
        .unwrap());
    manager.close_temporary_roles(1);
    assert!(!manager.has_temporary_role_references(1));
    assert!(!manager
        .peer_temporary_role_reference(2, 20, &cancel)
        .unwrap());
}

#[test]
fn aborted_publication_restores_old_references_and_commit_removes_retired_references() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    manager
        .prepare_temporary_roles(1, BTreeSet::from([20]), &cancel)
        .unwrap()
        .commit();
    let publication = manager
        .prepare_temporary_roles(1, BTreeSet::from([21]), &cancel)
        .unwrap();
    assert!(manager
        .peer_temporary_role_reference(2, 20, &cancel)
        .unwrap());
    assert!(manager
        .peer_temporary_role_reference(2, 21, &cancel)
        .unwrap());
    drop(publication);
    assert!(manager
        .peer_temporary_role_reference(2, 20, &cancel)
        .unwrap());
    assert!(!manager
        .peer_temporary_role_reference(2, 21, &cancel)
        .unwrap());
    manager
        .prepare_temporary_roles(1, BTreeSet::from([21]), &cancel)
        .unwrap()
        .commit();
    assert!(!manager
        .peer_temporary_role_reference(2, 20, &cancel)
        .unwrap());
    assert!(manager
        .peer_temporary_role_reference(2, 21, &cancel)
        .unwrap());
    // Dropping an unchanged publication must not undo the previously committed set.
    drop(
        manager
            .prepare_temporary_roles(1, BTreeSet::from([21]), &cancel)
            .unwrap(),
    );
    assert!(manager
        .peer_temporary_role_reference(2, 21, &cancel)
        .unwrap());
    manager
        .prepare_temporary_roles(1, BTreeSet::new(), &cancel)
        .unwrap()
        .commit();
    assert!(!manager.has_temporary_role_references(1));
}

#[test]
fn reference_capacity_is_shared_and_failure_preserves_existing_references() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    manager
        .prepare_temporary_roles(1, (1..=MAX_TEMPORARY_ROLE_REFERENCES).collect(), &cancel)
        .unwrap()
        .commit();
    let error = manager
        .prepare_temporary_roles(2, BTreeSet::from([20]), &cancel)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(!manager.has_temporary_role_references(2));
    assert!(manager
        .peer_temporary_role_reference(2, 20, &cancel)
        .unwrap());
    manager.close_temporary_roles(1);
    manager
        .prepare_temporary_roles(2, BTreeSet::from([20]), &cancel)
        .unwrap()
        .commit();
    assert!(manager
        .peer_temporary_role_reference(1, 20, &cancel)
        .unwrap());
}
