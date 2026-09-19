//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn serialized_document_watermarks_preserve_the_final_identity_without_wrapping() {
    let allocator = DocumentIdAllocator::new(None, [0; 16], [0; 16]).unwrap();
    assert!(!allocator.is_durable());
    let mut next = 1;
    assert_eq!(allocator.allocate(&mut next).unwrap(), 1);
    allocator.observe(&mut next, 900).unwrap();
    allocator.observe(&mut next, 1).unwrap();
    assert_eq!(allocator.allocate(&mut next).unwrap(), 901);
    next = u128::from(u64::MAX);
    assert_eq!(allocator.allocate(&mut next).unwrap(), u64::MAX);
    assert!(allocator.allocate(&mut next).is_err());
    assert_eq!(next, u128::from(u64::MAX) + 1);
    allocator.observe(&mut next, 0).unwrap();
    assert_eq!(next, u128::from(u64::MAX) + 1);
}

#[test]
fn invalid_document_watermarks_fail_without_rewriting_the_local_floor() {
    let allocator = DocumentIdAllocator::new(None, [0; 16], [0; 16]).unwrap();
    for invalid in [0, u128::from(u64::MAX) + 2, u128::MAX] {
        let mut next = invalid;
        assert!(allocator.synchronize(&mut next).is_err());
        assert_eq!(next, invalid);
    }
    let mut invalid = u128::MAX;
    assert!(allocator.observe(&mut invalid, 1).is_err());
    assert_eq!(invalid, u128::MAX);
}
