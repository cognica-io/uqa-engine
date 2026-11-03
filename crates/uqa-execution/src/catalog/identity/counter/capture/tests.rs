//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{mpsc, Arc};

#[test]
fn memory_catalog_oid_capture_preserves_uninitialized_wrapped_and_exhausted_positions() {
    for (position, expected) in [
        (None, Some(16_384)),
        (Some(16_384), Some(16_385)),
        (Some(u64::from(u32::MAX)), Some(16_384)),
        (Some(u64::from(u32::MAX) + 1), Some(16_385)),
        (Some(u64::MAX), None),
    ] {
        let source = CatalogOidCounter::from_position(position).unwrap();
        let control = StorageReadControl::with_limit(size_of::<Option<u64>>());
        let ((), saved) = source.capture_with(&control, || Ok(())).unwrap();
        assert_eq!(*saved.get().unwrap(), position);
        assert_eq!(control.memory().used(), size_of::<Option<u64>>());
        let restored = CatalogOidCounter::from_position(*saved.get().unwrap()).unwrap();
        for counter in [&source, &restored] {
            let result = counter.next_oid(None, || Ok(None));
            match expected {
                Some(oid) => assert_eq!(result.unwrap(), oid),
                None => assert!(result.unwrap_err().to_string().contains("exhausted")),
            }
        }
        assert_eq!(*saved.get().unwrap(), position);
        control.cancellation().cancel();
        assert!(saved.get().is_err());
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
    for position in [0, 1, 16_383] {
        assert!(CatalogOidCounter::from_position(Some(position)).is_err());
    }
}

#[test]
fn memory_catalog_oid_capture_orders_joint_registration_with_reservations() {
    let counter = Arc::new(CatalogOidCounter::from_position(Some(16_390)).unwrap());
    let weak = Arc::downgrade(&counter);
    let (start, proceed) = mpsc::channel();
    let (entered, started) = mpsc::channel();
    let (finished, reservation) = mpsc::channel();
    let writer = counter.clone();
    let worker = std::thread::spawn(move || {
        proceed.recv().unwrap();
        entered.send(()).unwrap();
        let oid = writer.next_oid(None, || panic!("initialized counter"));
        finished.send(oid.unwrap()).unwrap();
    });
    let control = StorageReadControl::with_limit(128);
    let (selected, saved) = counter
        .capture_with(&control, || {
            start.send(()).unwrap();
            started
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap();
            assert!(counter.position.try_lock().is_none());
            assert!(matches!(
                reservation.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            Ok(17_u64)
        })
        .unwrap();
    assert_eq!(selected, 17);
    assert_eq!(
        reservation
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap(),
        16_391
    );
    worker.join().unwrap();
    let other_control = StorageReadControl::with_limit(128);
    let ((), other) = counter.capture_with(&other_control, || Ok(())).unwrap();
    assert_eq!(*saved.get().unwrap(), Some(16_390));
    assert_eq!(*other.get().unwrap(), Some(16_391));
    drop(counter);
    assert!(weak.upgrade().is_none());
    control.cancellation().cancel();
    assert!(saved.get().is_err());
    assert_eq!(*other.get().unwrap(), Some(16_391));
    drop((saved, other));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(other_control.memory().used(), 0);
}

#[test]
fn memory_catalog_oid_capture_failures_leave_reservations_usable() {
    let counter = CatalogOidCounter::from_position(Some(16_384)).unwrap();
    let denied = StorageReadControl::with_limit(size_of::<Option<u64>>() - 1);
    assert!(counter
        .capture_with::<()>(&denied, || panic!("denied capture cannot register a cut"))
        .is_err());
    assert_eq!(denied.memory().used(), 0);
    let failed = StorageReadControl::with_limit(128);
    assert!(counter
        .capture_with::<()>(&failed, || Err(StorageBackendError::Other(
            "cut failed".into()
        )))
        .is_err());
    assert_eq!(failed.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(128);
    assert!(counter
        .capture_with(&cancelled, || {
            cancelled.cancellation().cancel();
            Ok(())
        })
        .is_err());
    assert_eq!(cancelled.memory().used(), 0);
    assert_eq!(
        counter
            .next_oid(None, || panic!("initialized counter"))
            .unwrap(),
        16_385
    );
    assert!(counter.position.try_lock().is_some());
}

#[test]
fn memory_catalog_oid_capture_cancels_while_allocation_admission_is_busy() {
    let counter = Arc::new(CatalogOidCounter::from_position(Some(16_384)).unwrap());
    let position = counter.position.lock();
    let control = StorageReadControl::with_limit(128);
    let (finished, outcome) = mpsc::channel();
    let capturing = counter.clone();
    let worker_control = control.clone();
    let worker = std::thread::spawn(move || {
        let failed = capturing
            .capture_with::<()>(&worker_control, || {
                panic!("counter admission is still held")
            })
            .is_err();
        finished.send(failed).unwrap();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    // The retained scalar allowance proves capture passed its first cancellation check and is waiting for the held allocation owner.
    while control.memory().used() == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(matches!(outcome.try_recv(), Err(mpsc::TryRecvError::Empty)));
    control.cancellation().cancel();
    assert!(outcome
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap());
    worker.join().unwrap();
    assert_eq!(*position, Some(16_384));
    assert_eq!(control.memory().used(), 0);
    drop(position);
    assert_eq!(
        counter
            .next_oid(None, || panic!("initialized counter"))
            .unwrap(),
        16_385
    );
}
