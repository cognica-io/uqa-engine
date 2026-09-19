//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` table-lock compatibility, combined acquisitions and savepoint ownership.

use super::*;

#[test]
fn enclosing_retention_preserves_only_the_requested_mode_across_savepoint_undo() {
    use RelationLockMode::{AccessExclusive, AccessShare, RowExclusive, Share};
    for already_held in [false, true] {
        let manager = RowLockManager::new();
        let table = manager.table_key("s");
        let cancel = uqa_core::CancellationToken::new();
        manager
            .acquire_relation(1, table, AccessExclusive, 1, &cancel)
            .unwrap();
        if already_held {
            manager
                .acquire_relation(1, table, RowExclusive, 1, &cancel)
                .unwrap();
        }
        manager
            .acquire_scoped_relation(1, table, RowExclusive, (1, 2), &cancel)
            .unwrap()
            .retain_at(0);
        manager.release_mark_above(1, 0);
        assert!(manager
            .try_acquire_relation(2, table, AccessShare, 0, &cancel)
            .unwrap());
        assert!(!manager
            .try_acquire_relation(2, table, Share, 0, &cancel)
            .unwrap());
        manager.release_session(1);
        assert!(manager
            .try_acquire_relation(2, table, AccessExclusive, 0, &cancel)
            .unwrap());
    }
}

#[test]
fn retained_binding_locks_keep_the_transaction_mark_and_failed_bindings_leave_it_alone() {
    use RelationLockMode::{RowExclusive, Share};
    let manager = RowLockManager::new();
    let table = manager.table_key("t");
    let cancel = uqa_core::CancellationToken::new();
    manager
        .acquire_relation(1, table, RowExclusive, 0, &cancel)
        .unwrap();
    manager
        .acquire_scoped_relation(1, table, Share, (1, 2), &cancel)
        .unwrap()
        .retain();
    assert!(!manager
        .try_acquire_relation(2, table, RowExclusive, 0, &cancel)
        .unwrap());
    assert!(manager
        .try_acquire_scoped_relation(2, table, Share, (0, 1), &cancel)
        .unwrap()
        .is_none());
    manager.release_mark_above(1, 1);
    assert!(!manager
        .try_acquire_relation(2, table, RowExclusive, 0, &cancel)
        .unwrap());
    manager.release_mark_above(1, 0);
    assert!(manager
        .try_acquire_scoped_relation(2, table, RowExclusive, (0, 1), &cancel)
        .unwrap()
        .is_some());
    assert!(!manager
        .try_acquire_relation(2, table, Share, 0, &cancel)
        .unwrap());
    manager.release_session(1);
    assert!(manager
        .try_acquire_relation(2, table, Share, 0, &cancel)
        .unwrap());
}

#[test]
fn conditional_relation_acquisition_matches_every_conflict_without_registering_waits() {
    let conflicts = [
        ".......X", "......XX", "....XXXX", "...XXXXX", "..XX.XXX", "..XXXXXX", ".XXXXXXX",
        "XXXXXXXX",
    ];
    let cancel = uqa_core::CancellationToken::new();
    for (held, expected) in RelationLockMode::ALL.into_iter().zip(conflicts) {
        let manager = RowLockManager::new();
        let table = manager.table_key("t");
        manager
            .acquire_relation(1, table, held, 0, &cancel)
            .unwrap();
        for (wanted, conflict) in RelationLockMode::ALL.into_iter().zip(expected.bytes()) {
            assert_eq!(
                manager
                    .try_acquire_relation(2, table, wanted, 0, &cancel)
                    .unwrap(),
                conflict != b'X',
                "{held:?}, {wanted:?}",
            );
            let state = manager.state.lock();
            assert!(state.waiting_relations.is_empty());
            assert!(state.advertised_waits.is_empty());
            assert_eq!(
                state.relations[&table].len(),
                if conflict == b'X' { 1 } else { 2 }
            );
            drop(state);
            manager.release_session(2);
        }
        manager.release_session(1);
        assert!(manager
            .try_acquire_relation(2, table, RelationLockMode::AccessExclusive, 0, &cancel)
            .unwrap());
    }
}

#[test]
fn failed_conditional_upgrade_preserves_modes_marks_and_does_not_report_a_deadlock() {
    use RelationLockMode::{RowExclusive, Share};
    let manager = RowLockManager::new();
    let table = manager.table_key("t");
    let cancel = uqa_core::CancellationToken::new();
    for session in [1, 2] {
        manager
            .acquire_relation(session, table, RowExclusive, 0, &cancel)
            .unwrap();
    }
    manager
        .state
        .lock()
        .waiting_relations
        .entry(2)
        .or_default()
        .insert(table, Share);
    assert!(!manager
        .try_acquire_relation(1, table, Share, 1, &cancel)
        .unwrap());
    {
        let state = manager.state.lock();
        assert!(!state.waiting_relations.contains_key(&1));
        assert!(state.advertised_waits.is_empty());
        let grant = state.relations[&table]
            .iter()
            .find(|grant| grant.session_id == 1)
            .unwrap();
        assert_eq!(grant.acquisitions.len(), 1);
        assert_eq!(grant.acquisitions[0].mode, RowExclusive);
        assert_eq!(grant.acquisitions[0].mark, 0);
    }
    manager.release_session(2);
    assert!(manager
        .try_acquire_relation(1, table, Share, 1, &cancel)
        .unwrap());
    assert!(manager
        .try_acquire_relation(1, table, RowExclusive, 2, &cancel)
        .unwrap());
    manager.release_mark_above(1, 0);
    assert!(manager
        .try_acquire_relation(2, table, RowExclusive, 0, &cancel)
        .unwrap());
    assert!(!manager
        .try_acquire_relation(2, table, Share, 0, &cancel)
        .unwrap());
    cancel.cancel();
    assert_eq!(
        manager
            .try_acquire_relation(1, table, RowExclusive, 0, &cancel)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}

#[test]
fn temporary_binding_locks_release_only_their_new_acquisition_even_on_unwind() {
    for held in [
        RelationLockMode::AccessShare,
        RelationLockMode::RowExclusive,
    ] {
        let manager = RowLockManager::new();
        let table = manager.table_key("t");
        let cancel = uqa_core::CancellationToken::new();
        manager
            .acquire_relation(1, table, held, 0, &cancel)
            .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _binding = manager
                .acquire_scoped_relation(1, table, RelationLockMode::AccessShare, (0, 1), &cancel)
                .unwrap();
            panic!("abort relation binding");
        }));
        assert!(result.is_err());
        let state = manager.state.lock();
        let grants = &state.relations[&table];
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].acquisitions.len(), 1);
        assert_eq!(grants[0].acquisitions[0].mode, held);
    }
}

#[test]
fn cancelled_or_invalid_temporary_binding_does_not_change_existing_locks() {
    let manager = RowLockManager::new();
    let table = manager.table_key("t");
    let cancel = uqa_core::CancellationToken::new();
    manager
        .acquire_relation(1, table, RelationLockMode::RowExclusive, 0, &cancel)
        .unwrap();
    assert!(manager
        .acquire_scoped_relation(1, table, RelationLockMode::AccessShare, (1, 1), &cancel)
        .is_err());
    cancel.cancel();
    assert!(matches!(
        manager.acquire_scoped_relation(1, table, RelationLockMode::AccessShare, (0, 1), &cancel),
        Err(SQLError::Cancelled(_))
    ));
    let state = manager.state.lock();
    assert_eq!(state.relations[&table][0].acquisitions.len(), 1);
    assert_eq!(
        state.relations[&table][0].acquisitions[0].mode,
        RelationLockMode::RowExclusive
    );
}

#[test]
fn all_eight_relation_modes_match_postgresql_table_13_2() {
    // https://www.postgresql.org/docs/18/explicit-locking.html#LOCKING-TABLES
    let conflicts = [
        ".......X", "......XX", "....XXXX", "...XXXXX", "..XX.XXX", "..XXXXXX", ".XXXXXXX",
        "XXXXXXXX",
    ];
    for (left, expected) in RelationLockMode::ALL.into_iter().zip(conflicts) {
        for (right, conflict) in RelationLockMode::ALL.into_iter().zip(expected.bytes()) {
            assert_eq!(
                left.conflicts_with(right),
                conflict == b'X',
                "{left:?}, {right:?}"
            );
            let manager = RowLockManager::new();
            let table = manager.table_key("t");
            let mut state = manager.state.lock();
            assert!(matches!(
                try_grant_relation(&mut state, 1, table, left, 0),
                RelationGrantAttempt::Granted
            ));
            assert_eq!(
                matches!(
                    try_grant_relation(&mut state, 2, table, right, 0),
                    RelationGrantAttempt::Conflict
                ),
                conflict == b'X'
            );
        }
    }
}

#[test]
fn mixed_relation_modes_retain_each_conflict_until_its_acquisition_is_released() {
    use RelationLockMode::{AccessShare, RowExclusive, Share};
    let manager = RowLockManager::new();
    let table = manager.table_key("t");
    let cancel = uqa_core::CancellationToken::new();
    manager
        .acquire_relation(1, table, RowExclusive, 0, &cancel)
        .unwrap();
    manager
        .acquire_relation(1, table, Share, 1, &cancel)
        .unwrap();
    manager
        .acquire_relation(1, table, RowExclusive, 2, &cancel)
        .unwrap();
    {
        let mut state = manager.state.lock();
        assert_eq!(state.relations[&table][0].acquisitions.len(), 2);
        for mode in [RowExclusive, Share] {
            assert!(matches!(
                try_grant_relation(&mut state, 2, table, mode, 0),
                RelationGrantAttempt::Conflict
            ));
        }
        assert!(matches!(
            try_grant_relation(&mut state, 3, table, AccessShare, 0),
            RelationGrantAttempt::Granted
        ));
    }
    manager.release_mark_above(1, 0);
    let mut state = manager.state.lock();
    assert!(matches!(
        try_grant_relation(&mut state, 2, table, Share, 0),
        RelationGrantAttempt::Conflict
    ));
    assert!(matches!(
        try_grant_relation(&mut state, 2, table, RowExclusive, 0),
        RelationGrantAttempt::Granted
    ));
    assert_eq!(state.relations[&table][0].acquisitions.len(), 1);
}

#[test]
fn relation_deadlock_detection_includes_every_held_mode() {
    use RelationLockMode::{RowExclusive, Share};
    let manager = RowLockManager::new();
    let first = manager.table_key("first");
    let second = manager.table_key("second");
    let cancel = uqa_core::CancellationToken::new();
    manager
        .acquire_relation(1, first, RowExclusive, 0, &cancel)
        .unwrap();
    manager
        .acquire_relation(1, first, Share, 0, &cancel)
        .unwrap();
    manager
        .acquire_relation(2, second, RowExclusive, 0, &cancel)
        .unwrap();
    let mut state = manager.state.lock();
    state
        .waiting_relations
        .entry(1)
        .or_default()
        .insert(second, Share);
    assert!(relation_deadlock_exists(&state, 2, first, Share));
}
