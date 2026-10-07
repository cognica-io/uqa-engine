//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation generations exclude stale metadata when native slots are reused.

use super::*;
use crate::row_locks::cross_process::{relation_mode_claim, relation_wait_claim};
use crate::row_locks::RelationLockMode;

#[test]
fn row_waits_with_identical_descriptors_follow_only_the_exact_identity() {
    use crate::row_locks::cross_process::{row_claim, RowByte, RowIdentity};
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("row_waits.db")).unwrap();
    let first = row_claim(
        RowIdentity::Relation {
            generation: 1,
            doc_id: 1,
        },
        RowByte::Row,
        true,
    );
    let second = row_claim(
        RowIdentity::Relation {
            generation: 1,
            doc_id: 2,
        },
        RowByte::Row,
        true,
    );
    assert_eq!(first.offset, second.offset);
    assert_ne!(first.row, second.row);
    coordinator.try_claim(1, &[first]).unwrap().unwrap();
    coordinator.try_claim(2, &[second]).unwrap().unwrap();
    coordinator.register_wait(2, first);
    assert_eq!(coordinator.wait_of(std::process::id(), 2), Some((first, 0)));
    assert!(!coordinator.wait_cycle_reaches_session(1, second, &|_| None));
    assert!(coordinator
        .wait_cycle_reaches_session(1, second, &|session| (session == 2).then_some(first)));
    coordinator.release(1, &[first]);
    assert!(!coordinator
        .wait_cycle_reaches_session(1, second, &|session| (session == 2).then_some(first)));
    coordinator.clear_wait(2);
    coordinator.release(2, &[second]);
}

#[test]
fn holder_and_wait_metadata_preserve_the_full_generation_and_access_mode() {
    for generation in [0, 1, 256, i64::MAX as u64] {
        for write in [false, true] {
            let holder = HolderSlot {
                pid: 23,
                session: 51,
                offset: 1024,
                write,
                generation,
            };
            let decoded = HolderSlot::decode(&holder.encode()).unwrap();
            assert_eq!(
                (
                    decoded.pid,
                    decoded.session,
                    decoded.offset,
                    decoded.write,
                    decoded.generation
                ),
                (23, 51, 1024, write, generation)
            );
            let wait = WaitSlot {
                row: None,
                pid: 23,
                session: 51,
                offset: 1024,
                write,
                generation,
            };
            let decoded = WaitSlot::decode(&wait.encode()).unwrap();
            assert_eq!(
                (
                    decoded.pid,
                    decoded.session,
                    decoded.offset,
                    decoded.write,
                    decoded.generation
                ),
                (23, 51, 1024, write, generation)
            );
        }
    }
}

#[test]
fn reused_identity_slots_exclude_stale_holders_and_waiters_even_with_a_live_pid() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = FileLockCoordinator::open(&directory.path().join("generations.db")).unwrap();
    let cancel = uqa_core::CancellationToken::new();
    let first = coordinator.pin_relation(b"first", &cancel).unwrap();
    let wait = relation_wait_claim(first.slot(), RelationLockMode::AccessExclusive);
    let generation = coordinator
        .current_relation_generation(wait.offset)
        .unwrap();
    let holder = HolderSlot {
        pid: std::process::id(),
        session: 17,
        offset: relation_mode_claim(first.slot(), RelationLockMode::AccessExclusive, false).offset,
        write: false,
        generation,
    };
    coordinator.write_holder_slot(0, Some(&holder));
    coordinator.write_slot(
        0,
        Some(&WaitSlot {
            row: None,
            pid: holder.pid,
            session: holder.session,
            offset: wait.offset,
            write: wait.write,
            generation,
        }),
    );
    assert_eq!(coordinator.holder_sessions(wait, generation).len(), 1);
    assert_eq!(
        coordinator.wait_of(holder.pid, holder.session),
        Some((wait, generation))
    );
    drop(first);
    let second = coordinator.pin_relation(b"second", &cancel).unwrap();
    assert_eq!(
        relation_wait_claim(second.slot(), RelationLockMode::AccessExclusive),
        wait
    );
    let current = coordinator
        .current_relation_generation(wait.offset)
        .unwrap();
    assert_ne!(current, generation);
    assert!(coordinator.holder_sessions(wait, current).is_empty());
    assert!(coordinator.wait_of(holder.pid, holder.session).is_none());
    coordinator
        .try_slot_claim(41, second.slot(), RelationLockMode::AccessExclusive)
        .unwrap()
        .unwrap();
    assert_eq!(
        coordinator
            .local_holders_conflicting(wait, generation)
            .len(),
        0
    );
    assert_eq!(
        coordinator.local_holders_conflicting(wait, current),
        vec![41]
    );
    coordinator.release(
        41,
        &[relation_mode_claim(
            second.slot(),
            RelationLockMode::AccessExclusive,
            false,
        )],
    );
}

#[test]
fn a_cold_registry_epoch_recovers_corruption_and_clears_only_relation_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("epoch.db");
    let coordinator = FileLockCoordinator::open(&path).unwrap();
    let cancel = uqa_core::CancellationToken::new();
    let identity = coordinator.pin_relation(b"old identity", &cancel).unwrap();
    let claim = relation_wait_claim(identity.slot(), RelationLockMode::AccessExclusive);
    let generation = coordinator
        .current_relation_generation(claim.offset)
        .unwrap();
    for (index, offset, generation) in [(0, claim.offset, generation), (1, 10_000, 0)] {
        let holder = HolderSlot {
            pid: std::process::id(),
            session: 17,
            offset,
            write: true,
            generation,
        };
        coordinator.write_holder_slot(index, Some(&holder));
        coordinator.write_slot(
            index,
            Some(&WaitSlot {
                row: None,
                pid: holder.pid,
                session: holder.session,
                offset,
                write: true,
                generation,
            }),
        );
    }
    drop(identity);
    let registry = coordinator.relation_path.clone();
    drop(coordinator);
    // Simulate a host failure in disposable coordination metadata, with no surviving native attachment owner.
    std::fs::write(registry, b"corrupt registry").unwrap();
    let reopened = FileLockCoordinator::open(&path).unwrap();
    let identity = reopened.pin_relation(b"new identity", &cancel).unwrap();
    assert_eq!(reopened.current_relation_generation(claim.offset), Some(1));
    assert_eq!(identity.slot(), 0);
    assert!(reopened.read_holder_slot(0).is_none());
    assert!(reopened.read_slot(0).is_none());
    assert_eq!(reopened.read_holder_slot(1).unwrap().offset, 10_000);
    assert_eq!(reopened.read_slot(1).unwrap().offset, 10_000);
}
