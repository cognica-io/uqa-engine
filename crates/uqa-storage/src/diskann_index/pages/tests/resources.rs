//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn open_and_fragment_workspace_failures_unwind_the_original_allowance() {
    let fixture = fixture(1024, 2, 1);
    let physical = MemoryBudget::new(65_536);
    let work = StorageReadControl::with_limit(65_536);
    let source = Arc::new(fixture.memory(&physical, &work).unwrap());
    let mut passed = 0;
    let mut rejected = 0;
    for limit in [0, 100, 1024, 4096, 16_384, 65_536] {
        let control = StorageReadControl::with_limit(limit);
        match fixture.reader(source.clone(), limits(0), &control) {
            Ok(reader) => {
                passed += 1;
                drop(reader);
            }
            Err(StorageBackendError::Memory(_)) => rejected += 1,
            Err(error) => panic!("unexpected error {error}"),
        }
        assert_eq!(control.memory().used(), 0);
    }
    assert!(passed > 0 && rejected > 0);
    let reader = fixture.reader(source.clone(), limits(0), &work).unwrap();
    passed = 0;
    rejected = 0;
    for limit in [0, 100, 4095, 8192, 16_384, 65_536] {
        let control = StorageReadControl::with_limit(limit);
        match reader.read_node(1, &control) {
            Ok(node) => {
                passed += 1;
                drop(node);
            }
            Err(StorageBackendError::Memory(_)) => rejected += 1,
            Err(error) => panic!("unexpected error {error}"),
        }
        assert_eq!(control.memory().used(), 0);
    }
    assert!(passed > 0 && rejected > 0);
    for (resident, record, flight) in [
        (1, 65_536, PAGE_BYTES),
        (65_536, 1, PAGE_BYTES),
        (65_536, 65_536, PAGE_BYTES - 1),
    ] {
        let control = StorageReadControl::with_limit(65_536);
        let mut limits = limits(0);
        limits.resident_bytes = resident;
        limits.max_record_bytes = record;
        limits.max_in_flight_page_bytes = flight;
        assert!(fixture.reader(source.clone(), limits, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn cache_component_limits_cannot_enlarge_the_retained_parent_allowance() {
    let fixture = fixture(8, 4, 0);
    let physical = MemoryBudget::new(65_536);
    let probe = StorageReadControl::with_limit(65_536);
    let source = Arc::new(fixture.memory(&physical, &probe).unwrap());
    drop(fixture.reader(source.clone(), limits(0), &probe).unwrap());
    let owner = StorageReadControl::with_limit(probe.memory().peak());
    let reader = fixture.reader(source, limits(usize::MAX), &owner).unwrap();
    let retained = owner.memory().used();
    let query = StorageReadControl::with_limit(65_536);
    let pages = reader.read_pages(&[0], &query).unwrap();
    assert_eq!(reader.cache_bytes(), 0);
    assert_eq!(owner.memory().used(), retained);
    assert!(query.memory().used() >= PAGE_BYTES);
    drop(pages);
    assert_eq!(query.memory().used(), 0);
    drop(reader);
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn cancellation_and_failed_staging_never_create_a_partially_readable_source() {
    let fixture = fixture(8, 4, 1);
    let physical = MemoryBudget::new(65_536);
    let work = StorageReadControl::with_limit(65_536);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &work).unwrap()));
    let reader = fixture.reader(source.clone(), limits(8192), &work).unwrap();
    let query = StorageReadControl::with_limit(65_536);
    query.cancellation().cancel();
    assert!(matches!(
        reader.read_node(0, &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        reader.read_pages(&[], &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        reader.visit_side(&query, &mut |_| Ok(())),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 0);
    let records = source.records.load(Ordering::Relaxed);
    assert!(matches!(
        fixture.reader(source.clone(), limits(0), &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(source.records.load(Ordering::Relaxed), records);
    let failed = MemoryBudget::new(65_536);
    let mut bad = super::fixture(8, 4, 1);
    bad.graph[0][150] ^= 1;
    assert!(bad.memory(&failed, &work).is_err());
    assert_eq!(failed.used(), 0);
    let mut builder = DiskANNMemoryBuilder::new(generation(), &failed);
    builder.write_graph_page(0, &bad.graph[0], &work).unwrap();
    let retained = failed.used();
    assert!(builder.write_graph_page(0, &bad.graph[0], &work).is_err());
    assert_eq!(failed.used(), retained);
    assert!(builder.finish(bad.manifest, &work).is_err());
    assert_eq!(failed.used(), 0);
    assert!(!reader.read_pages(&[0], &work).unwrap().is_empty());
}
