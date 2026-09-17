//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` table-lock compatibility, combined acquisitions and savepoint ownership.

use super::*;

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
