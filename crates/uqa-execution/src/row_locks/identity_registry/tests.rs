//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{LockAcquire, LockRequest, RowLockKey, RowLockManager};
use uqa_core::CancellationToken;
use uqa_sql::ast::{LockStrength, LockWait};

fn digest(number: u64) -> [u8; 32] {
    let mut digest = [0; 32];
    digest[24..].copy_from_slice(&number.to_be_bytes());
    digest
}

fn request(key: u64, session_id: u64, cancel: &CancellationToken) -> LockRequest<'_> {
    LockRequest {
        session_id,
        key: RowLockKey {
            table: key,
            doc_id: 0,
        },
        strength: LockStrength::ForUpdate,
        mark: 0,
        wait: LockWait::Block,
        cancel,
        relation: "keys",
    }
}

#[test]
fn released_statement_key_reservations_leave_no_identity_metadata() {
    let directory = tempfile::tempdir().unwrap();
    for persistent in [false, true] {
        let manager = if persistent {
            RowLockManager::for_database_file(&directory.path().join("keys.db"))
        } else {
            RowLockManager::new()
        };
        let cancel = CancellationToken::new();
        for number in 0..256 {
            let retained = manager.retain_key_reservation(digest(number));
            manager
                .acquire(&request(retained.table_key(), 1, &cancel))
                .unwrap();
            drop(retained);
            assert_eq!(manager.identities.retained_counts().0, 1);
            manager.release_session(1);
            assert_eq!(manager.identities.retained_counts().0, 0);
        }
        let (entries, reverse, forward_capacity, reverse_capacity) =
            manager.identities.retained_counts();
        assert_eq!((entries, reverse), (0, 0));
        assert!(forward_capacity <= 128 && reverse_capacity <= 128);
    }
}

#[test]
fn released_key_batches_reclaim_high_water_capacity_and_preserve_table_ids() {
    let manager = RowLockManager::new();
    let permanent = manager.table_key("public.kept");
    let cancel = CancellationToken::new();
    let retained = (0..4096)
        .map(|n| manager.retain_key_reservation(digest(n)))
        .collect::<Vec<_>>();
    let requests = retained
        .iter()
        .map(|key| request(key.table_key(), 1, &cancel))
        .collect::<Vec<_>>();
    assert_eq!(
        manager.acquire_batch(&requests).unwrap().len(),
        requests.len()
    );
    drop(retained);
    assert_eq!(manager.identities.retained_counts().0, 4097);
    manager.release_session(1);
    let (entries, reverse, forward_capacity, reverse_capacity) =
        manager.identities.retained_counts();
    assert_eq!((entries, reverse), (1, 1));
    assert!(forward_capacity <= 128 && reverse_capacity <= 128);
    assert_eq!(manager.table_key("public.kept"), permanent);
    assert_eq!(&*manager.table_name(permanent), "public.kept");
}

#[test]
fn key_identity_survives_duplicate_acquisition_upgrade_and_partial_undo() {
    let directory = tempfile::tempdir().unwrap();
    let manager = RowLockManager::for_database_file(&directory.path().join("undo.db"));
    let cancel = CancellationToken::new();
    let retained = manager.retain_key_reservation(digest(1));
    let mut wanted = request(retained.table_key(), 1, &cancel);
    wanted.strength = LockStrength::ForKeyShare;
    manager.acquire(&wanted).unwrap();
    assert!(matches!(
        manager.acquire(&wanted).unwrap(),
        LockAcquire::Granted {
            acquisition: None,
            ..
        }
    ));
    wanted.strength = LockStrength::ForUpdate;
    wanted.mark = 1;
    let LockAcquire::Granted {
        acquisition: Some(upgrade),
        ..
    } = manager.acquire(&wanted).unwrap()
    else {
        panic!("new upgrade");
    };
    drop(retained);
    manager.rollback_acquisition(upgrade);
    assert_eq!(manager.identities.retained_counts().0, 1);
    let rebound = manager.retain_key_reservation(digest(1));
    assert_eq!(rebound.table_key(), wanted.key.table);
    manager.acquire(&wanted).unwrap();
    drop(rebound);
    manager.release_mark_above(1, 0);
    assert_eq!(manager.identities.retained_counts().0, 1);
    manager.release_session(1);
    assert_eq!(manager.identities.retained_counts().0, 0);
    assert_ne!(
        manager.retain_key_reservation(digest(1)).table_key(),
        wanted.key.table
    );
}

#[test]
fn failed_key_batches_keep_only_granted_prefixes_and_cancelled_requests_leave_nothing() {
    let manager = RowLockManager::new();
    let cancel = CancellationToken::new();
    let held = manager.retain_key_reservation(digest(1));
    manager
        .acquire(&request(held.table_key(), 2, &cancel))
        .unwrap();
    drop(held);
    let retained = (0..3)
        .map(|n| manager.retain_key_reservation(digest(n)))
        .collect::<Vec<_>>();
    let wanted = retained
        .iter()
        .map(|key| {
            let mut request = request(key.table_key(), 1, &cancel);
            request.wait = LockWait::NoWait;
            request
        })
        .collect::<Vec<_>>();
    assert_eq!(
        manager.acquire_batch(&wanted).unwrap_err().sqlstate(),
        Some("55P03")
    );
    drop(retained);
    assert_eq!(manager.identities.retained_counts().0, 2);
    manager.release_session(1);
    manager.release_session(2);
    assert_eq!(manager.identities.retained_counts().0, 0);
    let retained = manager.retain_key_reservation(digest(100));
    cancel.cancel();
    assert_eq!(
        manager
            .acquire(&request(retained.table_key(), 1, &cancel))
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(retained);
    assert_eq!(manager.identities.retained_counts().0, 0);
}

#[test]
fn a_waiter_keeps_key_identity_after_its_previous_holder_releases() {
    let manager = Arc::new(RowLockManager::new());
    let cancel = CancellationToken::new();
    let retained = manager.retain_key_reservation(digest(1));
    let wanted = request(retained.table_key(), 1, &cancel);
    manager.acquire(&wanted).unwrap();
    let row = wanted.key;
    drop(retained);
    let peer = Arc::clone(&manager);
    let waiter = std::thread::spawn(move || {
        let cancel = CancellationToken::new();
        let retained = peer.retain_key_reservation(digest(1));
        assert_eq!(retained.table_key(), row.table);
        let result = peer
            .acquire(&request(retained.table_key(), 2, &cancel))
            .unwrap();
        assert!(matches!(result, LockAcquire::Granted { waited: true, .. }));
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !manager.waiting_for_row(2, row) {
        assert!(
            std::time::Instant::now() < deadline,
            "waiter did not register"
        );
        std::thread::yield_now();
    }
    manager.release_session(1);
    waiter.join().unwrap();
    assert_eq!(manager.identities.retained_counts().0, 1);
    manager.release_session(2);
    assert_eq!(manager.identities.retained_counts().0, 0);
}

#[test]
fn permanent_key_callers_preserve_the_same_identity_after_scoped_owners_leave() {
    let registry = IdentityRegistry::default();
    let retained = registry.key(digest(1));
    let table = registry.permanent(LockRelationIdentity::KeyReservation(digest(1)));
    assert_eq!(retained.table_key(), table);
    drop(retained);
    assert_eq!(registry.key(digest(1)).table_key(), table);
    assert_eq!(registry.retained_counts().0, 1);
}

#[test]
fn concurrent_key_handles_share_complete_identity_and_reclaim_every_wave() {
    let registry = Arc::new(IdentityRegistry::default());
    let barrier = Arc::new(std::sync::Barrier::new(4));
    std::thread::scope(|threads| {
        for _ in 0..4 {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            threads.spawn(move || {
                for number in 0..256 {
                    let first = registry.key(digest(number));
                    barrier.wait();
                    let second = registry.key(digest(number));
                    assert_eq!(first.table_key(), second.table_key());
                    barrier.wait();
                    drop(first);
                    drop(second);
                    barrier.wait();
                }
            });
        }
    });
    assert_eq!(registry.retained_counts().0, 0);
}
