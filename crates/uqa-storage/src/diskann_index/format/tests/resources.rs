//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn failed_node_allocation_releases_partial_vectors_and_retains_source_bytes() {
    let source = StorageReadControl::with_limit(16_384);
    let layout = DiskANNNodeLayout::new(3, 2, 3).unwrap();
    let bytes = layout.encode_node(&input(1), &source).unwrap();
    let retained = source.memory().used();
    let mut passed = 0;
    let mut rejected = 0;
    for limit in 0..128 {
        let control = StorageReadControl::with_limit(limit);
        match layout.decode_node(1, &bytes, &control) {
            Ok(node) => {
                passed += 1;
                drop(node);
            }
            Err(StorageBackendError::Memory(_)) => rejected += 1,
            Err(error) => panic!("unexpected error: {error}"),
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(source.memory().used(), retained);
    }
    assert!(passed > 0 && rejected > 0);
    let limited = StorageReadControl::with_limit(layout.slot_bytes() - 1);
    assert!(matches!(
        layout.encode_node(&input(1), &limited),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(limited.memory().used(), 0);
}

#[test]
fn page_buffers_are_admitted_before_allocation_and_all_operations_observe_cancellation() {
    let source = StorageReadControl::with_limit(16_384);
    let (layout, bytes) = packed(&source);
    let page = decode_page(generation(), layout, 0, &bytes, &source).unwrap();
    for limit in [0, PAGE_BYTES - 1] {
        let control = StorageReadControl::with_limit(limit);
        assert!(matches!(
            encode_page(generation(), layout, 0, page.payload(), &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(PAGE_BYTES);
    let copy = encode_page(generation(), layout, 0, page.payload(), &control).unwrap();
    assert_eq!(control.memory().used(), PAGE_BYTES);
    drop(copy);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        encode_page(generation(), layout, 0, page.payload(), &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        decode_page(generation(), layout, 0, &bytes, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        layout.encode_node(&input(1), &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        layout.decode_node(0, &page.payload()[..layout.slot_bytes()], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
