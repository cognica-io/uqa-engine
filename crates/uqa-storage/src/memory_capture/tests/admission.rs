//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn source(value: u64) -> Scalar {
    Scalar {
        value,
        calls: Arc::default(),
        scratch: 0,
    }
}

#[test]
fn admitted_registration_failure_keeps_the_original_source_usable() {
    let registry = MemoryCaptureRegistry::new();
    let bytes = size_of::<Budgeted<RegisteredCell<Scalar>>>();
    let denied = StorageReadControl::with_limit(bytes - 1);
    let mut cell = MemoryCaptureCell::new(source(7));
    assert!(matches!(
        cell.register_with_control(&registry, &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
    assert_eq!(cell.read().value, 7);
    assert!(cell.key().is_err());
    cell.write().value = 8;

    let input = StorageReadControl::with_limit(bytes);
    cell.register_with_control(&registry, &input).unwrap();
    assert_eq!(input.memory().used(), bytes);
    let original = cell.key().unwrap();
    cell.register_with_control(&registry, &denied).unwrap();
    assert!(original.same_source(&cell.key().unwrap()));
    assert!(cell
        .register_with_control(&MemoryCaptureRegistry::new(), &input)
        .is_err());
    assert_eq!(cell.read().value, 8);
    input.cancellation().cancel();
    let output = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&output).unwrap();
    cell.write().value = 9;
    assert_eq!(**original.captured(&cut).unwrap(), 8);
    assert_eq!(cell.read().value, 9);
    drop(cell);
    assert_eq!(input.memory().used(), 0);
    assert_eq!(**original.captured(&cut).unwrap(), 8);
    drop(cut);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn admitted_source_headers_live_until_promoted_readers_retire() {
    let registry = MemoryCaptureRegistry::new();
    let input = StorageReadControl::with_limit(4096);
    let cell = registry.cell_with_control(source(7), &input).unwrap();
    let key = cell.key().unwrap();
    let bytes = input.memory().used();
    assert_eq!(bytes, key.retained_source_bytes());
    let promoted = key.source.upgrade().unwrap();
    let output = StorageReadControl::with_limit(4096);
    let cut = registry.capture(&output).unwrap();
    drop(cell);
    assert_eq!(input.memory().used(), bytes);
    let saved = promoted.captured(&cut).unwrap();
    assert_eq!(**saved, 7);
    drop(promoted);
    assert_eq!(input.memory().used(), 0);
    assert!(key.source.upgrade().is_none());
    assert!(Arc::ptr_eq(&saved, &key.captured(&cut).unwrap()));
    drop((cut, saved));
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn admitted_registration_observes_cancellation_while_publication_is_blocked() {
    let registry = MemoryCaptureRegistry::new();
    let input = StorageReadControl::with_limit(4096);
    let held = registry.0.admission.write();
    let (waiting, observed) = mpsc::channel();
    let mut cell = thread::scope(|scope| {
        let registry = &registry;
        let input = &input;
        let worker = scope.spawn(move || {
            ON_SOURCE_WAIT.set(Some(Box::new(move || waiting.send(()).unwrap())));
            let mut cell = MemoryCaptureCell::new(source(7));
            assert!(matches!(
                cell.register_with_control(registry, input),
                Err(StorageBackendError::Cancelled(_))
            ));
            cell
        });
        observed.recv().unwrap();
        input.cancellation().cancel();
        worker.join().unwrap()
    });
    assert_eq!(input.memory().used(), 0);
    assert_eq!(cell.read().value, 7);
    assert!(cell.key().is_err());
    drop(held);
    let fresh = StorageReadControl::with_limit(4096);
    cell.register_with_control(&registry, &fresh).unwrap();
    cell.write().value = 8;
    assert_eq!(cell.read().value, 8);
    drop(cell);
    assert_eq!(fresh.memory().used(), 0);
}
