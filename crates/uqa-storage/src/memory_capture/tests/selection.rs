//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryBudget;

#[test]
fn selected_union_preserves_overlapping_identities_once_and_excludes_other_sources() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(1 << 20);
    let first = scalar(&registry, 10);
    let second = scalar(&registry, 20);
    let excluded = registry
        .cell(Scalar {
            value: 30,
            calls: Arc::default(),
            scratch: usize::MAX,
        })
        .unwrap();
    let left = registry
        .sources(&owner)
        .with_source(&first.key().unwrap())
        .unwrap();
    let right = left.with_source(&second.key().unwrap()).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry
        .capture_selected_union(&left, &right, &control)
        .unwrap();
    let admitted = control.memory().used();
    excluded.write().value = 31;
    assert!(excluded.captured(&cut).is_err());
    assert_eq!(excluded.read().calls.load(Ordering::Relaxed), 0);
    assert_eq!(control.memory().used(), admitted);
    first.write().value = 11;
    second.write().value = 21;
    assert_eq!(first.read().calls.load(Ordering::Relaxed), 1);
    assert_eq!(second.read().calls.load(Ordering::Relaxed), 1);
    let first_key = first.key().unwrap();
    let second_key = second.key().unwrap();
    drop((first, second, excluded, left, right, registry));
    assert_eq!(**first_key.captured(&cut).unwrap(), 10);
    assert_eq!(**second_key.captured(&cut).unwrap(), 20);
    drop(cut);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(owner.used(), 0);
}

#[test]
fn selected_union_admits_complete_roots_before_registration_and_captures_in_constant_work() {
    let mut allocations = Vec::new();
    for count in [1, 4096] {
        let registry = MemoryCaptureRegistry::new();
        let owner = MemoryBudget::new(16 << 20);
        let cells: Vec<_> = (0..count).map(|n| scalar(&registry, n)).collect();
        let mut left = registry.sources(&owner);
        for cell in &cells {
            left = left.with_source(&cell.key().unwrap()).unwrap();
        }
        let catalog = scalar(&registry, 99);
        let right = registry
            .sources(&owner)
            .with_source(&catalog.key().unwrap())
            .unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let mut cut = None;
        let measured = allocation_counter::measure(|| {
            cut = Some(
                registry
                    .capture_selected_union(&left, &right, &control)
                    .unwrap(),
            );
        });
        allocations.push((measured.bytes_total, measured.count_total));
        let admitted = control.memory().used();
        assert!(cells
            .iter()
            .all(|cell| cell.read().calls.load(Ordering::Relaxed) == 0));
        assert_eq!(catalog.read().calls.load(Ordering::Relaxed), 0);
        let denied = StorageReadControl::with_limit(admitted - 1);
        assert!(matches!(
            registry.capture_selected_union(&left, &right, &denied),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(denied.memory().used(), 0);
        let foreign = MemoryCaptureRegistry::new();
        let foreign_root = foreign.sources(&owner);
        assert!(registry
            .capture_selected_union(&left, &foreign_root, &denied)
            .is_err());
        assert_eq!(denied.memory().used(), 0);
        let exact = StorageReadControl::with_limit(admitted);
        let exact_cut = registry
            .capture_selected_union(&left, &right, &exact)
            .unwrap();
        assert_eq!(exact.memory().used(), admitted);
        drop((cut, exact_cut, left, right, cells, catalog, registry));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(exact.memory().used(), 0);
        assert_eq!(owner.used(), 0);
    }
    assert_eq!(allocations[0], allocations[1]);
    assert!(allocations[1].0 < 4096, "{allocations:?}");
}

#[test]
fn selected_sources_ignore_unselected_writes_retirement_and_failed_reads() {
    let registry = MemoryCaptureRegistry::new();
    let committed = scalar(&registry, 7);
    let private = registry
        .cell(Scalar {
            value: 19,
            calls: Arc::default(),
            scratch: usize::MAX,
        })
        .unwrap();
    let private_calls = private.read().calls.clone();
    let committed_key = committed.key().unwrap();
    let private_key = private.key().unwrap();
    let membership = MemoryBudget::new(4096);
    let sources = registry
        .sources(&membership)
        .with_source(&committed_key)
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = registry.capture_selected(&sources, &control).unwrap();
    let admission = control.memory().used();
    assert!(private.captured(&cut).is_err());
    assert!(private_key.captured(&cut).is_err());
    assert!(private_key.retained(&cut).is_err());
    let held = private.write();
    assert!(private_key.captured(&cut).is_err());
    drop(held);
    let writes = allocation_counter::measure(|| {
        private.write().value = 20;
        private.try_write().unwrap().value = 21;
    });
    assert_eq!(writes.bytes_total, 0);
    assert_eq!(control.memory().used(), admission);
    drop(private);
    assert_eq!(private_calls.load(Ordering::Relaxed), 0);
    assert!(private_key.captured(&cut).is_err());
    cut.check().unwrap();
    committed.write().value = 8;
    assert_eq!(committed.read().calls.load(Ordering::Relaxed), 1);
    drop((committed, registry, sources));
    assert_eq!(**committed_key.captured(&cut).unwrap(), 7);
    drop(cut);
    assert_eq!(membership.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn selection_successors_preserve_exact_incarnations_and_independent_cuts() {
    let registry = MemoryCaptureRegistry::new();
    let first = scalar(&registry, 10);
    let second = scalar(&registry, 20);
    let first_key = first.key().unwrap();
    let second_key = second.key().unwrap();
    let owner = MemoryBudget::new(8192);
    let original = registry.sources(&owner).with_source(&first_key).unwrap();
    let first_control = StorageReadControl::with_limit(8192);
    let old = registry
        .capture_selected(&original, &first_control)
        .unwrap();
    let next = original
        .with_source(&second_key)
        .unwrap()
        .without_source(&first_key)
        .unwrap();
    let second_control = StorageReadControl::with_limit(8192);
    let current = registry.capture_selected(&next, &second_control).unwrap();
    assert_eq!(original.len(), 1);
    assert_eq!(next.len(), 1);
    assert!(!next.is_empty());
    assert!(second_key.captured(&old).is_err());
    assert!(first_key.captured(&current).is_err());
    let later = scalar(&registry, 30);
    assert!(later.key().unwrap().captured(&old).is_err());
    let empty = next.without_source(&second_key).unwrap();
    assert!(empty.is_empty());
    let empty_control = StorageReadControl::with_limit(4096);
    let empty_cut = registry.capture_selected(&empty, &empty_control).unwrap();
    first.write().value = 11;
    second.write().value = 21;
    assert_eq!(**first_key.captured(&old).unwrap(), 10);
    assert_eq!(**second_key.captured(&current).unwrap(), 20);
    assert!(first_key.captured(&empty_cut).is_err());
    empty_cut.check().unwrap();
    assert_eq!(first.read().calls.load(Ordering::Relaxed), 1);
    assert_eq!(second.read().calls.load(Ordering::Relaxed), 1);
    drop((first, second, later, registry, original, next, empty));
    assert_eq!(**first_key.captured(&old).unwrap(), 10);
    assert_eq!(**second_key.captured(&current).unwrap(), 20);
    drop((old, current, empty_cut));
    assert_eq!(owner.used(), 0);
    assert_eq!(first_control.memory().used(), 0);
    assert_eq!(second_control.memory().used(), 0);
    assert_eq!(empty_control.memory().used(), 0);
}

#[test]
fn selection_domain_and_construction_failures_preserve_the_original_root() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(4096);
    let first = scalar(&registry, 1);
    let second = scalar(&registry, 2);
    let third = scalar(&registry, 3);
    let sources = registry
        .sources(&owner)
        .with_source(&first.key().unwrap())
        .unwrap()
        .with_source(&second.key().unwrap())
        .unwrap();
    let reserved = owner.used();
    let hold = owner.reserve(owner.available()).unwrap();
    assert!(sources.with_source(&third.key().unwrap()).is_err());
    assert!(sources.without_source(&second.key().unwrap()).is_err());
    assert_eq!(sources.len(), 2);
    let unchanged = allocation_counter::measure(|| {
        assert_eq!(sources.with_source(&first.key().unwrap()).unwrap().len(), 2);
        assert_eq!(
            sources.without_source(&third.key().unwrap()).unwrap().len(),
            2
        );
    });
    assert_eq!(unchanged.bytes_total, 0);
    drop(hold);
    assert_eq!(owner.used(), reserved);
    let foreign = MemoryCaptureRegistry::new();
    let foreign_cell = scalar(&foreign, 4);
    let key = foreign_cell.key().unwrap();
    assert!(sources.with_source(&key).is_err());
    assert!(sources.without_source(&key).is_err());
    let control = StorageReadControl::with_limit(8192);
    assert!(foreign.capture_selected(&sources, &control).is_err());
    assert_eq!(control.memory().used(), 0);
    assert!(foreign.0.captures.lock().is_empty());
    let insufficient = StorageReadControl::with_limit(1);
    assert!(registry.capture_selected(&sources, &insufficient).is_err());
    assert_eq!(insufficient.memory().used(), 0);
    assert!(registry.0.captures.lock().is_empty());
    let cut = registry.capture_selected(&sources, &control).unwrap();
    assert_eq!(**first.captured(&cut).unwrap(), 1);
    assert_eq!(**second.captured(&cut).unwrap(), 2);
    assert!(third.captured(&cut).is_err());
    drop((cut, sources));
    assert_eq!(owner.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn selection_failure_and_cancellation_release_membership_without_affecting_peers() {
    let registry = MemoryCaptureRegistry::new();
    let cell = registry
        .cell(Scalar {
            value: 42,
            calls: Arc::default(),
            scratch: 8192,
        })
        .unwrap();
    let owner = MemoryBudget::new(4096);
    let sources = registry
        .sources(&owner)
        .with_source(&cell.key().unwrap())
        .unwrap();
    let limited = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(16384);
    let cancelled = StorageReadControl::with_limit(16384);
    let bad = registry.capture_selected(&sources, &limited).unwrap();
    let saved = registry.capture_selected(&sources, &good).unwrap();
    let stopped = registry.capture_selected(&sources, &cancelled).unwrap();
    drop(sources);
    cancelled.cancellation().cancel();
    cell.write().value = 43;
    assert!(matches!(bad.check(), Err(StorageBackendError::Memory(_))));
    assert!(matches!(
        stopped.check(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cell.read().value, 43);
    assert_eq!(cell.read().calls.load(Ordering::Relaxed), 2);
    assert_eq!(**cell.captured(&saved).unwrap(), 42);
    assert!(owner.used() > 0);
    good.cancellation().cancel();
    assert!(saved.check().is_err());
    assert_eq!(owner.used(), 0);
    assert!(registry.0.captures.lock().is_empty());
    assert!(cell.captured(&bad).is_err());
    drop((bad, saved, stopped));
    assert_eq!(limited.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
    assert_eq!(cancelled.memory().used(), 0);
}

#[test]
fn selected_cut_registration_has_constant_allocation_and_visits_no_sources() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(16 * 1024 * 1024);
    let cells: Vec<_> = (0..8192).map(|value| scalar(&registry, value)).collect();
    let mut sources = registry
        .sources(&owner)
        .with_source(&cells[0].key().unwrap())
        .unwrap();
    let control = StorageReadControl::with_limit(16 * 1024 * 1024);
    let mut cut = None;
    let one = allocation_counter::measure(|| {
        cut = Some(registry.capture_selected(&sources, &control).unwrap());
    });
    let one_admission = control.memory().used();
    drop(cut.take());
    for cell in &cells[1..] {
        sources = sources.with_source(&cell.key().unwrap()).unwrap();
    }
    let many = allocation_counter::measure(|| {
        cut = Some(registry.capture_selected(&sources, &control).unwrap());
    });
    assert_eq!(one.count_total, many.count_total);
    assert_eq!(one.bytes_total, many.bytes_total);
    assert!(control.memory().used() > one_admission);
    assert!(cells
        .iter()
        .all(|cell| cell.read().calls.load(Ordering::Relaxed) == 0));
    drop((cut, sources));
    assert_eq!(owner.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn selected_registration_releases_every_incomplete_admission() {
    let registry = MemoryCaptureRegistry::new();
    let cell = scalar(&registry, 1);
    let owner = MemoryBudget::new(4096);
    let sources = registry
        .sources(&owner)
        .with_source(&cell.key().unwrap())
        .unwrap();
    let owner_used = owner.used();
    let complete = StorageReadControl::with_limit(4096);
    let cut = registry.capture_selected(&sources, &complete).unwrap();
    let required = complete.memory().used();
    drop(cut);
    for bytes in 0..required {
        let control = StorageReadControl::with_limit(bytes);
        assert!(matches!(
            registry.capture_selected(&sources, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0, "limit {bytes}");
        assert_eq!(owner.used(), owner_used, "limit {bytes}");
        assert!(registry.0.captures.lock().is_empty());
    }
    let exact = StorageReadControl::with_limit(required);
    let cut = registry.capture_selected(&sources, &exact).unwrap();
    assert_eq!(exact.memory().used(), required);
    drop(cut);
    assert_eq!(exact.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(4096);
    cancelled.cancellation().cancel();
    assert!(matches!(
        registry.capture_selected(&sources, &cancelled),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
    assert_eq!(cell.read().calls.load(Ordering::Relaxed), 0);
    drop(sources);
    assert_eq!(owner.used(), 0);
}
