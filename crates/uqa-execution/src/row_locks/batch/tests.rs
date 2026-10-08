//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batched grants retain individual ownership, rollback marks and ordinary wait behavior.

use crate::row_locks::{LockAcquire, LockRequest, RowLockKey, RowLockManager};
use uqa_core::CancellationToken;
use uqa_sql::ast::{LockStrength, LockWait};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod process;

fn requests<'a>(
    manager: &RowLockManager,
    cancel: &'a CancellationToken,
    session_id: u64,
    count: u64,
) -> Vec<LockRequest<'a>> {
    let table = manager.table_key("public.batch");
    (1..=count)
        .map(|doc_id| LockRequest {
            session_id,
            key: RowLockKey { table, doc_id },
            strength: LockStrength::ForUpdate,
            mark: 1,
            wait: LockWait::NoWait,
            cancel,
            relation: "batch",
        })
        .collect()
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn batches_bound_claim_table_arbitration_and_preserve_every_grant() {
    for count in [2, 64, 65, 128] {
        let directory = tempfile::tempdir().unwrap();
        let manager = RowLockManager::for_database_file(&directory.path().join("claims.db"));
        let coordinator = manager.coordinator().unwrap().unwrap();
        let cancel = CancellationToken::new();
        let wanted = requests(&manager, &cancel, 1, count);
        let initial_locks = coordinator
            .row_claim_table_locks
            .load(std::sync::atomic::Ordering::Relaxed);
        let granted = manager.acquire_batch(&wanted).unwrap();
        assert_eq!(granted.len(), count as usize);
        assert!(granted.iter().all(|grant| matches!(
            grant,
            LockAcquire::Granted {
                acquisition: Some(_),
                waited: false,
                foreign_waited: false
            }
        )));
        let locks = coordinator
            .row_claim_table_locks
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(locks - initial_locks, count.div_ceil(64) as usize);
        assert_eq!(manager.state.lock().rows.len(), count as usize);
        let again = manager.acquire_batch(&wanted).unwrap();
        assert!(again.iter().all(|grant| matches!(
            grant,
            LockAcquire::Granted {
                acquisition: None,
                ..
            }
        )));
        assert_eq!(
            coordinator
                .row_claim_table_locks
                .load(std::sync::atomic::Ordering::Relaxed),
            locks
        );
        let mut competing = requests(&manager, &cancel, 2, count);
        for request in &mut competing {
            request.wait = LockWait::SkipLocked;
        }
        assert!(manager
            .acquire_batch(&competing)
            .unwrap()
            .iter()
            .all(|grant| *grant == LockAcquire::Skipped));
        manager.release_mark_above(1, 0);
        assert!(manager.state.lock().rows.is_empty());
        assert!(manager
            .acquire_batch(&competing)
            .unwrap()
            .iter()
            .all(|grant| matches!(
                grant,
                LockAcquire::Granted {
                    acquisition: Some(_),
                    ..
                }
            )));
        manager.release_session(2);
        assert!(manager.state.lock().rows.is_empty());
    }
}

#[test]
fn a_conflicting_batch_preserves_only_the_original_acquisition_prefix() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    let holder = requests(&manager, &cancel, 1, 2);
    manager.acquire(&holder[1]).unwrap();
    let wanted = requests(&manager, &cancel, 2, 3);
    assert_eq!(
        manager.acquire_batch(&wanted).unwrap_err().sqlstate(),
        Some("55P03")
    );
    let state = manager.state.lock();
    assert_eq!(state.rows[&wanted[0].key][0].session_id, 2);
    assert_eq!(state.rows[&wanted[1].key][0].session_id, 1);
    assert!(!state.rows.contains_key(&wanted[2].key));
    assert!(state.waiting.is_empty() && state.advertised_waits.is_empty());
}

#[test]
fn repeated_rows_and_upgrades_keep_savepoint_ownership() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    let mut wanted = requests(&manager, &cancel, 1, 3);
    wanted[1].key = wanted[0].key;
    wanted[0].strength = LockStrength::ForKeyShare;
    wanted[0].mark = 0;
    let result = manager.acquire_batch(&wanted).unwrap();
    assert!(result.iter().all(|grant| matches!(
        grant,
        LockAcquire::Granted {
            acquisition: Some(_),
            ..
        }
    )));
    manager.release_mark_above(1, 0);
    let state = manager.state.lock();
    assert_eq!(state.rows.len(), 1);
    assert_eq!(
        state.rows[&wanted[0].key][0].effective_strength(),
        LockStrength::ForKeyShare
    );
    drop(state);
    manager.release_session(1);
    assert!(manager.state.lock().rows.is_empty());
}

#[test]
fn a_cancelled_batch_retains_no_provisional_grants() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    let wanted = requests(&manager, &cancel, 1, 3);
    cancel.cancel();
    assert_eq!(
        manager.acquire_batch(&wanted).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(manager.state.lock().rows.is_empty());
    assert!(manager.acquire_batch(&[]).unwrap().is_empty());
}

#[test]
fn a_blocked_batch_waits_in_order_and_reports_the_waited_request() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    let holder = requests(&manager, &cancel, 1, 2);
    manager.acquire(&holder[1]).unwrap();
    let mut wanted = requests(&manager, &cancel, 2, 3);
    for request in &mut wanted {
        request.wait = LockWait::Block;
    }
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| manager.acquire_batch(&wanted));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !manager.state.lock().waiting.contains_key(&2)
            && !worker.is_finished()
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let waiting = manager.state.lock().waiting.contains_key(&2);
        manager.release_session(1);
        let granted = worker.join().unwrap().unwrap();
        assert!(waiting);
        for (index, grant) in granted.into_iter().enumerate() {
            assert!(
                matches!(grant, LockAcquire::Granted { waited, acquisition: Some(_), .. } if waited == (index == 1))
            );
        }
    });
    manager.release_session(2);
    let state = manager.state.lock();
    assert!(state.rows.is_empty() && state.waiting.is_empty());
}
