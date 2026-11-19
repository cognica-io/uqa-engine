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
    DocumentIdAllocator::observe(&mut next, 900).unwrap();
    DocumentIdAllocator::observe(&mut next, 1).unwrap();
    assert_eq!(allocator.allocate(&mut next).unwrap(), 901);
    next = u128::from(u64::MAX);
    assert_eq!(allocator.allocate(&mut next).unwrap(), u64::MAX);
    assert!(allocator.allocate(&mut next).is_err());
    assert_eq!(next, u128::from(u64::MAX) + 1);
    DocumentIdAllocator::observe(&mut next, 0).unwrap();
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
    assert!(DocumentIdAllocator::observe(&mut invalid, 1).is_err());
    assert_eq!(invalid, u128::MAX);
}

#[test]
fn restored_document_floors_read_durable_reservations_without_allocating() {
    struct Source {
        floor: Option<u64>,
        fail: bool,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl IdentifierAllocator for Source {
        fn identifier_watermark(&self, namespace: &[u8]) -> StorageBackendResult<Option<u64>> {
            assert_eq!(namespace, document_namespace([1; 16], [2; 16]));
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.fail {
                Err(StorageBackendError::Other(
                    "selected allocator read failed".into(),
                ))
            } else {
                Ok(self.floor)
            }
        }

        fn allocate_identifiers(
            &self,
            _: &[u8],
            _: IdentifierRequest,
        ) -> StorageBackendResult<crate::mvcc::IdentifierAllocation> {
            panic!("reading a restored floor must not reserve or observe an identity")
        }
    }

    for (durable, local, expected) in [
        (None, 1, 1),
        (Some(0), 1, 1),
        (Some(900), 1, 901),
        (Some(900), 1000, 1000),
        (Some(u64::MAX), 1, u128::from(u64::MAX) + 1),
    ] {
        let source = Source {
            floor: durable,
            fail: false,
            reads: std::sync::atomic::AtomicUsize::new(0),
        };
        let allocator = DocumentIdAllocator::new(Some(&source), [1; 16], [2; 16]).unwrap();
        assert_eq!(allocator.next_id_floor(local).unwrap(), expected);
        for invalid in [0, u128::from(u64::MAX) + 2, u128::MAX] {
            assert!(allocator.next_id_floor(invalid).is_err());
        }
        assert_eq!(source.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
    let serialized = DocumentIdAllocator::new(None, [0; 16], [0; 16]).unwrap();
    assert_eq!(serialized.next_id_floor(901).unwrap(), 901);
    let failing = Source {
        floor: None,
        fail: true,
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    let allocator = DocumentIdAllocator::new(Some(&failing), [1; 16], [2; 16]).unwrap();
    assert_eq!(
        allocator.next_id_floor(1).unwrap_err().to_string(),
        "selected allocator read failed"
    );
}
