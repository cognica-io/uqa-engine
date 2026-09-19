//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission retains live predecessors without blocking view destruction during collection.

use super::*;

#[test]
fn live_clones_retain_the_oldest_boundary_and_collection_allows_drop() {
    let registry = Arc::new(SnapshotRegistry::default());
    let control = StorageReadControl::with_limit(1 << 20);
    let first = registry
        .capture(&control, || Ok(CommitSequence::from_u64(2)))
        .unwrap();
    let retained = control.memory().used();
    assert!(retained > 0);
    let clone = registry
        .capture(&control, || Ok(CommitSequence::from_u64(2)))
        .unwrap();
    assert!(Arc::ptr_eq(&first, &clone));
    assert_eq!(control.memory().used(), retained);
    let newer = registry
        .capture(&control, || Ok(CommitSequence::from_u64(4)))
        .unwrap();
    drop(first);
    registry
        .reclaim(&control, |oldest| {
            assert_eq!(oldest, Some(CommitSequence::from_u64(2)));
            drop(clone);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        registry.reclaim(&control, Ok).unwrap(),
        Some(newer.sequence())
    );
    drop(newer);
    assert_eq!(registry.reclaim(&control, Ok).unwrap(), None);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_or_cancelled_capture_publishes_no_lease_and_horizons_reject_future_anchors() {
    let registry = Arc::new(SnapshotRegistry::default());
    let zero = StorageReadControl::with_limit(0);
    assert!(registry
        .capture(&zero, || Ok(CommitSequence::from_u64(1)))
        .is_err());
    let control = StorageReadControl::with_limit(1 << 20);
    assert_eq!(registry.reclaim(&control, Ok).unwrap(), None);
    assert!(registry
        .capture(&control, || Err(VersionError::WrongDatabase))
        .is_err());
    assert_eq!(registry.reclaim(&control, Ok).unwrap(), None);
    let horizon = ReclamationHorizon::new(
        CommitSequence::from_u64(4),
        Some(CommitSequence::from_u64(2)),
    )
    .unwrap();
    assert!(horizon.anchor(CommitSequence::INITIAL).is_err());
    assert!(horizon.anchor(CommitSequence::from_u64(3)).is_err());
    assert_eq!(
        horizon.anchor(CommitSequence::from_u64(2)).unwrap(),
        CommitSequence::from_u64(2)
    );
    assert!(ReclamationHorizon::new(
        CommitSequence::from_u64(1),
        Some(CommitSequence::from_u64(2))
    )
    .is_err());
    control.cancellation().cancel();
    assert!(registry
        .capture(&control, || panic!("cancelled capture reached persistence"))
        .is_err());
}

#[test]
fn capture_cannot_read_a_sequence_during_collection_and_waits_are_cancellable() {
    use std::sync::mpsc;
    use std::time::Duration;
    let registry = Arc::new(SnapshotRegistry::default());
    let control = StorageReadControl::with_limit(1 << 20);
    let (entered, collecting) = mpsc::channel();
    let (release, released) = mpsc::channel();
    std::thread::scope(|scope| {
        let collector_registry = Arc::clone(&registry);
        let collector_control = control.clone();
        let collector = scope.spawn(move || {
            collector_registry.reclaim(&collector_control, |_| {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(30)).unwrap();
                Ok(())
            })
        });
        collecting.recv_timeout(Duration::from_secs(30)).unwrap();
        let cancelled = StorageReadControl::with_limit(1 << 20);
        let (starting, started) = mpsc::channel();
        let (completed, result) = mpsc::channel();
        let waiting_registry = Arc::clone(&registry);
        let waiting_control = cancelled.clone();
        let waiting = scope.spawn(move || {
            starting.send(()).unwrap();
            let result = waiting_registry.capture(&waiting_control, || {
                panic!("capture entered a collector's physical boundary")
            });
            completed.send(result.is_err()).unwrap();
        });
        started.recv_timeout(Duration::from_secs(30)).unwrap();
        cancelled.cancellation().cancel();
        assert!(result.recv_timeout(Duration::from_secs(30)).unwrap());
        waiting.join().unwrap();
        release.send(()).unwrap();
        collector.join().unwrap().unwrap();
    });
    assert!(registry
        .capture(&control, || Ok(CommitSequence::from_u64(1)))
        .is_ok());
}
