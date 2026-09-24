//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{DatabaseId, VersionError};

fn id(allocation: u64) -> SerializableTransactionId {
    SerializableTransactionId::new(DatabaseId::from_bytes([9; 16]), [3; 16], allocation).unwrap()
}

#[test]
fn participants_retain_the_database_and_gate_after_all_adapters_close() {
    let state = Arc::new(LocalSerializableState::default());
    let owner = Arc::new(());
    let retained_state = Arc::downgrade(&state);
    let retained_owner = Arc::downgrade(&owner);
    let control = StorageReadControl::with_limit(1 << 20);
    let participant = state
        .with_admission(&owner, &control, |leases| leases.retain(id(1), &control))
        .unwrap();
    let nested = participant.clone();
    drop((state, owner, participant));
    let state = retained_state.upgrade().unwrap();
    let owner = retained_owner.upgrade().unwrap();
    state
        .with_admission(&owner, &control, |leases| {
            assert!(leases.is_alive(id(1), &control)?);
            // Final release must not enter the gate already held by this callback.
            drop(nested);
            assert!(!leases.is_alive(id(1), &control)?);
            leases.reclaim();
            Ok(())
        })
        .unwrap();
    assert_eq!(control.memory().used(), 0);
    drop((state, owner));
    assert!(retained_state.upgrade().is_none());
    assert!(retained_owner.upgrade().is_none());
}

#[test]
fn failed_first_admission_does_not_impose_its_allowance_on_later_callers() {
    let state = Arc::new(LocalSerializableState::default());
    let owner = Arc::new(());
    let empty = StorageReadControl::with_limit(0);
    assert!(matches!(
        state.with_admission(&owner, &empty, |leases| leases.retain(id(1), &empty)),
        Err(VersionError::Memory(_))
    ));
    let control = StorageReadControl::with_limit(1 << 20);
    let actor = state
        .with_admission(&owner, &control, |leases| leases.retain(id(2), &control))
        .unwrap();
    assert!(control.memory().used() > 0);
    assert_eq!(empty.memory().used(), 0);
    drop(actor);
    state
        .with_admission(&owner, &control, |leases| {
            leases.reclaim();
            Ok(())
        })
        .unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn waiting_for_local_admission_is_cancellable_before_running_the_operation() {
    use std::{sync::mpsc, time::Duration};

    let state = Arc::new(LocalSerializableState::default());
    let owner = Arc::new(());
    let control = StorageReadControl::with_limit(1 << 20);
    let waiting = StorageReadControl::with_limit(1 << 20);
    let (entered, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    std::thread::scope(|scope| {
        let holding_state = &state;
        let holding_owner = &owner;
        let holding_control = &control;
        let held = scope.spawn(move || {
            holding_state.with_admission(holding_owner, holding_control, |_| {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(30)).unwrap();
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        let (started, starting) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let waiting_state = &state;
        let waiting_owner = &owner;
        let waiting_control = &waiting;
        let waiter = scope.spawn(move || {
            started.send(()).unwrap();
            finished
                .send(
                    waiting_state.with_admission(waiting_owner, waiting_control, |_| {
                        panic!("cancelled waiter reached the operation")
                    }),
                )
                .unwrap();
        });
        starting.recv_timeout(Duration::from_secs(30)).unwrap();
        waiting.cancellation().cancel();
        let result: VersionResult<()> = result.recv_timeout(Duration::from_secs(30)).unwrap();
        release.send(()).unwrap();
        held.join().unwrap().unwrap();
        waiter.join().unwrap();
        assert!(
            matches!(result, Err(VersionError::Cancelled(_))),
            "{result:?}"
        );
    });
    assert_eq!(waiting.memory().used(), 0);
}
