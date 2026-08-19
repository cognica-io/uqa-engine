//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_granted(result: Result<LockAcquire, SQLError>) {
    assert!(matches!(result.unwrap(), LockAcquire::Granted { .. }));
}

fn wait_until_registered(manager: &RowLockManager, session_id: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if manager.state.lock().waiting.contains_key(&session_id) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "row-lock waiter was not registered"
        );
        std::thread::yield_now();
    }
}

#[test]
fn postgresql_tuple_lock_conflicts_match_strength_matrix() {
    use LockStrength::{ForKeyShare, ForNoKeyUpdate, ForShare, ForUpdate};
    assert!(!lock_strengths_conflict(ForKeyShare, ForKeyShare));
    assert!(!lock_strengths_conflict(ForKeyShare, ForShare));
    assert!(!lock_strengths_conflict(ForKeyShare, ForNoKeyUpdate));
    assert!(lock_strengths_conflict(ForKeyShare, ForUpdate));
    assert!(!lock_strengths_conflict(ForShare, ForShare));
    assert!(lock_strengths_conflict(ForShare, ForNoKeyUpdate));
    assert!(lock_strengths_conflict(ForShare, ForUpdate));
    assert!(lock_strengths_conflict(ForNoKeyUpdate, ForNoKeyUpdate));
    assert!(lock_strengths_conflict(ForNoKeyUpdate, ForUpdate));
    assert!(lock_strengths_conflict(ForUpdate, ForUpdate));
}

#[test]
fn acquire_reports_40p01_on_a_wait_for_cycle() {
    let manager = RowLockManager::new();
    let left = manager.allocate_session();
    let right = manager.allocate_session();
    let row_one = RowLockKey {
        table: 1,
        doc_id: 1,
    };
    let row_two = RowLockKey {
        table: 1,
        doc_id: 2,
    };
    let cancel = uqa_core::CancellationToken::new();
    let grant = |session_id, key| {
        manager.acquire(&LockRequest {
            session_id,
            key,
            strength: LockStrength::ForUpdate,
            mark: 0,
            wait: uqa_sql::ast::LockWait::Block,
            cancel: &cancel,
            relation: "accounts",
        })
    };
    assert_granted(grant(left, row_one));
    assert_granted(grant(right, row_two));

    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            manager.acquire(&LockRequest {
                session_id: left,
                key: row_two,
                strength: LockStrength::ForUpdate,
                mark: 0,
                wait: uqa_sql::ast::LockWait::Block,
                cancel: &cancel,
                relation: "accounts",
            })
        });
        wait_until_registered(&manager, left);
        let error = grant(right, row_one).unwrap_err();
        assert_eq!(error.sqlstate(), Some("40P01"));
        manager.release_session(right);
        assert_granted(waiter.join().unwrap());
    });
    manager.release_session(left);
}

#[test]
fn savepoint_rollback_restores_the_pre_upgrade_strength() {
    let manager = RowLockManager::new();
    let holder = manager.allocate_session();
    let contender = manager.allocate_session();
    let key = RowLockKey {
        table: 1,
        doc_id: 1,
    };
    let cancel = uqa_core::CancellationToken::new();
    let acquire = |session_id, strength, mark, wait| {
        manager.acquire(&LockRequest {
            session_id,
            key,
            strength,
            mark,
            wait,
            cancel: &cancel,
            relation: "accounts",
        })
    };

    assert_granted(acquire(
        holder,
        LockStrength::ForKeyShare,
        0,
        uqa_sql::ast::LockWait::Block,
    ));
    assert_granted(acquire(
        holder,
        LockStrength::ForUpdate,
        1,
        uqa_sql::ast::LockWait::Block,
    ));
    manager.release_mark_above(holder, 0);
    assert_granted(acquire(
        contender,
        LockStrength::ForNoKeyUpdate,
        0,
        uqa_sql::ast::LockWait::NoWait,
    ));
}

#[test]
fn rolling_back_one_candidate_does_not_release_an_earlier_lock() {
    let manager = RowLockManager::new();
    let holder = manager.allocate_session();
    let contender = manager.allocate_session();
    let key = RowLockKey {
        table: 1,
        doc_id: 1,
    };
    let cancel = uqa_core::CancellationToken::new();
    let request = |session_id, strength, wait| LockRequest {
        session_id,
        key,
        strength,
        mark: 0,
        wait,
        cancel: &cancel,
        relation: "accounts",
    };
    let first = manager
        .acquire(&request(
            holder,
            LockStrength::ForShare,
            uqa_sql::ast::LockWait::Block,
        ))
        .unwrap();
    assert!(matches!(
        first,
        LockAcquire::Granted {
            acquisition: Some(_),
            ..
        }
    ));
    let repeated = manager
        .acquire(&request(
            holder,
            LockStrength::ForShare,
            uqa_sql::ast::LockWait::Block,
        ))
        .unwrap();
    assert!(matches!(
        repeated,
        LockAcquire::Granted {
            acquisition: None,
            ..
        }
    ));
    let error = manager
        .acquire(&request(
            contender,
            LockStrength::ForNoKeyUpdate,
            uqa_sql::ast::LockWait::NoWait,
        ))
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
}

#[test]
fn deadlock_edges_ignore_compatible_holders() {
    let waiter = 1;
    let compatible_holder = 2;
    let conflicting_holder = 3;
    let wanted = RowLockKey {
        table: 1,
        doc_id: 1,
    };
    let held_by_waiter = RowLockKey {
        table: 1,
        doc_id: 2,
    };
    let grant = |session_id, strength, acquisition_id| LockGrant {
        session_id,
        acquisitions: vec![MarkedStrength {
            acquisition_id,
            strength,
            mark: 0,
        }],
    };
    let state = LockTable {
        rows: HashMap::from([
            (
                wanted,
                vec![
                    grant(compatible_holder, LockStrength::ForKeyShare, 1),
                    grant(conflicting_holder, LockStrength::ForShare, 2),
                ],
            ),
            (
                held_by_waiter,
                vec![grant(waiter, LockStrength::ForUpdate, 3)],
            ),
        ]),
        waiting: HashMap::from([(
            compatible_holder,
            HashMap::from([(held_by_waiter, LockStrength::ForUpdate)]),
        )]),
        relations: HashMap::new(),
        waiting_relations: HashMap::new(),
        advertised_waits: HashMap::new(),
        changes: Vec::new(),
        change_epoch: 0,
        active_change_observers: 0,
    };
    assert!(!deadlock_exists(
        &state,
        waiter,
        wanted,
        LockStrength::ForNoKeyUpdate,
    ));
}

#[test]
fn cancellation_removes_the_registered_wait_edge() {
    let manager = RowLockManager::new();
    let holder = manager.allocate_session();
    let waiter = manager.allocate_session();
    let key = RowLockKey {
        table: 1,
        doc_id: 1,
    };
    let holder_cancel = uqa_core::CancellationToken::new();
    assert_granted(manager.acquire(&LockRequest {
        session_id: holder,
        key,
        strength: LockStrength::ForUpdate,
        mark: 0,
        wait: uqa_sql::ast::LockWait::Block,
        cancel: &holder_cancel,
        relation: "accounts",
    }));
    let waiter_cancel = uqa_core::CancellationToken::new();
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            manager.acquire(&LockRequest {
                session_id: waiter,
                key,
                strength: LockStrength::ForUpdate,
                mark: 0,
                wait: uqa_sql::ast::LockWait::Block,
                cancel: &waiter_cancel,
                relation: "accounts",
            })
        });
        wait_until_registered(&manager, waiter);
        waiter_cancel.cancel();
        let error = waiting.join().unwrap().unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
    });
    assert!(!manager.state.lock().waiting.contains_key(&waiter));
}

#[test]
fn changed_row_versions_are_dropped_with_the_last_lock() {
    let manager = RowLockManager::new();
    let holder = manager.allocate_session();
    let key = RowLockKey {
        table: manager.table_key("public.accounts"),
        doc_id: 1,
    };
    let cancel = uqa_core::CancellationToken::new();
    assert_granted(manager.acquire(&LockRequest {
        session_id: holder,
        key,
        strength: LockStrength::ForUpdate,
        mark: 0,
        wait: uqa_sql::ast::LockWait::Block,
        cancel: &cancel,
        relation: "accounts",
    }));
    manager
        .publish_row_changes(
            holder,
            [PendingRowChange {
                key,
                kind: PendingRowChangeKind::Update,
            }],
        )
        .unwrap();
    assert_eq!(manager.current_row_version("public.accounts", 1), 1);
    manager.release_session(holder);
    assert_eq!(manager.current_row_version("public.accounts", 1), 0);
    assert!(manager.state.lock().changes.is_empty());
}

#[test]
fn row_change_epochs_ignore_unrelated_commits() {
    let manager = Arc::new(RowLockManager::new());
    let writer = manager.allocate_session();
    let _observation = manager.begin_change_observation();
    let baseline = RowChangeBaseline {
        epoch: manager.current_change_epoch(),
        cross_sequence: 0,
    };
    let table = manager.table_key("public.accounts");
    let target = RowLockKey { table, doc_id: 1 };
    let unrelated = RowLockKey { table, doc_id: 2 };
    let changed_after = |doc_id, baseline| {
        manager
            .conflicting_change_target_after(
                "public.accounts",
                doc_id,
                baseline,
                LockStrength::ForUpdate,
            )
            .unwrap()
            != RowChangeTarget::Unchanged
    };

    manager
        .publish_row_changes(
            writer,
            [PendingRowChange {
                key: unrelated,
                kind: PendingRowChangeKind::Update,
            }],
        )
        .unwrap();
    assert!(!changed_after(1, baseline));
    assert!(changed_after(2, baseline));

    let retry_baseline = RowChangeBaseline {
        epoch: manager.current_change_epoch(),
        cross_sequence: 0,
    };
    manager
        .publish_row_changes(
            writer,
            [PendingRowChange {
                key: target,
                kind: PendingRowChangeKind::Update,
            }],
        )
        .unwrap();
    assert!(changed_after(1, retry_baseline));
    assert!(!changed_after(2, retry_baseline));
}

#[test]
fn key_share_ignores_compatible_non_key_mutations() {
    let manager = Arc::new(RowLockManager::new());
    let writer = manager.allocate_session();
    let _observation = manager.begin_change_observation();
    let baseline = RowChangeBaseline {
        epoch: manager.current_change_epoch(),
        cross_sequence: 0,
    };
    let key = RowLockKey {
        table: manager.table_key("public.accounts"),
        doc_id: 1,
    };
    let cancel = uqa_core::CancellationToken::new();
    assert_granted(manager.acquire(&LockRequest {
        session_id: writer,
        key,
        strength: LockStrength::ForNoKeyUpdate,
        mark: 0,
        wait: uqa_sql::ast::LockWait::Block,
        cancel: &cancel,
        relation: "accounts",
    }));
    manager
        .publish_row_changes(
            writer,
            [PendingRowChange {
                key,
                kind: PendingRowChangeKind::Update,
            }],
        )
        .unwrap();
    assert_eq!(
        manager
            .conflicting_change_target_after(
                "public.accounts",
                1,
                baseline,
                LockStrength::ForKeyShare
            )
            .unwrap(),
        RowChangeTarget::Unchanged
    );
    assert_eq!(
        manager
            .conflicting_change_target_after("public.accounts", 1, baseline, LockStrength::ForShare)
            .unwrap(),
        RowChangeTarget::Present(1)
    );
}

#[test]
fn conflicting_change_targets_follow_primary_key_rewrites() {
    let manager = Arc::new(RowLockManager::new());
    let writer = manager.allocate_session();
    let _observation = manager.begin_change_observation();
    let baseline = RowChangeBaseline {
        epoch: manager.current_change_epoch(),
        cross_sequence: 0,
    };
    let table = manager.table_key("public.accounts");
    let old = RowLockKey { table, doc_id: 1 };
    let new = RowLockKey { table, doc_id: 2 };

    manager
        .publish_row_changes(
            writer,
            [
                PendingRowChange {
                    key: old,
                    kind: PendingRowChangeKind::Delete,
                },
                PendingRowChange {
                    key: new,
                    kind: PendingRowChangeKind::Insert,
                },
                PendingRowChange {
                    key: old,
                    kind: PendingRowChangeKind::Rewrite(new),
                },
            ],
        )
        .unwrap();
    assert_eq!(
        manager
            .conflicting_change_target_after(
                "public.accounts",
                1,
                baseline,
                LockStrength::ForUpdate,
            )
            .unwrap(),
        RowChangeTarget::Present(2)
    );
}

#[test]
fn delete_then_reinsert_of_the_same_key_terminates_the_old_generation() {
    let manager = Arc::new(RowLockManager::new());
    let writer = manager.allocate_session();
    let _observation = manager.begin_change_observation();
    let baseline = RowChangeBaseline {
        epoch: manager.current_change_epoch(),
        cross_sequence: 0,
    };
    let key = RowLockKey {
        table: manager.table_key("public.accounts"),
        doc_id: 1,
    };
    manager
        .publish_row_changes(
            writer,
            [
                PendingRowChange {
                    key,
                    kind: PendingRowChangeKind::Delete,
                },
                PendingRowChange {
                    key,
                    kind: PendingRowChangeKind::Insert,
                },
            ],
        )
        .unwrap();
    assert_eq!(
        manager
            .conflicting_change_target_after(
                "public.accounts",
                1,
                baseline,
                LockStrength::ForUpdate,
            )
            .unwrap(),
        RowChangeTarget::Deleted
    );
}

#[test]
fn primary_key_rewrite_chains_keep_commit_order() {
    let manager = Arc::new(RowLockManager::new());
    let writer = manager.allocate_session();
    let _observation = manager.begin_change_observation();
    let baseline = RowChangeBaseline {
        epoch: manager.current_change_epoch(),
        cross_sequence: 0,
    };
    let table = manager.table_key("public.accounts");
    let three = RowLockKey { table, doc_id: 3 };
    let two = RowLockKey { table, doc_id: 2 };
    let one = RowLockKey { table, doc_id: 1 };
    manager
        .publish_row_changes(
            writer,
            [
                PendingRowChange {
                    key: three,
                    kind: PendingRowChangeKind::Delete,
                },
                PendingRowChange {
                    key: two,
                    kind: PendingRowChangeKind::Insert,
                },
                PendingRowChange {
                    key: three,
                    kind: PendingRowChangeKind::Rewrite(two),
                },
                PendingRowChange {
                    key: two,
                    kind: PendingRowChangeKind::Delete,
                },
                PendingRowChange {
                    key: one,
                    kind: PendingRowChangeKind::Insert,
                },
                PendingRowChange {
                    key: two,
                    kind: PendingRowChangeKind::Rewrite(one),
                },
            ],
        )
        .unwrap();
    assert_eq!(
        manager
            .conflicting_change_target_after(
                "public.accounts",
                3,
                baseline,
                LockStrength::ForUpdate,
            )
            .unwrap(),
        RowChangeTarget::Present(1)
    );
}

#[test]
fn a_new_row_rewritten_in_its_inserting_transaction_has_no_old_generation() {
    let table = 1;
    let old = RowLockKey { table, doc_id: 3 };
    let new = RowLockKey { table, doc_id: 2 };
    let normalized = normalize_pending_row_changes([
        PendingRowChange {
            key: old,
            kind: PendingRowChangeKind::Insert,
        },
        PendingRowChange {
            key: old,
            kind: PendingRowChangeKind::Delete,
        },
        PendingRowChange {
            key: new,
            kind: PendingRowChangeKind::Insert,
        },
        PendingRowChange {
            key: old,
            kind: PendingRowChangeKind::Rewrite(new),
        },
    ]);
    assert!(normalized.is_empty(), "unexpected changes: {normalized:?}");
}

#[cfg(any(unix, windows))]
#[test]
fn durable_change_journal_retains_history_beyond_the_old_ring_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let manager = RowLockManager::for_database_file(&directory.path().join("journal.db"));
    let writer = manager.allocate_session();
    let cancel = uqa_core::CancellationToken::new();
    let baseline = manager
        .begin_change_snapshot(&cancel)
        .unwrap()
        .baseline()
        .unwrap();
    let table = manager.table_key("public.accounts");
    let mut changes = (10_000..15_000)
        .map(|doc_id| PendingRowChange {
            key: RowLockKey { table, doc_id },
            kind: PendingRowChangeKind::Update,
        })
        .collect::<Vec<_>>();
    changes.push(PendingRowChange {
        key: RowLockKey { table, doc_id: 1 },
        kind: PendingRowChangeKind::Update,
    });
    manager.publish_row_changes(writer, changes).unwrap();
    assert_eq!(
        manager
            .conflicting_change_target_after(
                "public.accounts",
                1,
                baseline,
                LockStrength::ForUpdate,
            )
            .unwrap(),
        RowChangeTarget::Present(1)
    );
}

#[test]
fn change_gate_wait_observes_statement_cancellation() {
    let manager = Arc::new(RowLockManager::new());
    let holder_cancel = uqa_core::CancellationToken::new();
    let _publication = manager.begin_change_publication(&holder_cancel).unwrap();
    let waiter_manager = Arc::clone(&manager);
    let waiter_cancel = uqa_core::CancellationToken::new();
    let cancel = waiter_cancel.clone();
    let waiter = std::thread::spawn(move || {
        waiter_manager
            .begin_change_snapshot(&waiter_cancel)
            .map(|_| ())
    });

    std::thread::sleep(Duration::from_millis(100));
    cancel.cancel();
    let error = waiter.join().unwrap().unwrap_err();

    assert_eq!(error.sqlstate(), Some("57014"));
}
