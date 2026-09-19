//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared document-allocation schedules through the catalog/backend capability boundary.

use super::DocumentIdAllocator;
use crate::{PersistentStorageSession, StorageBackendResult, StorageSavepointId};

/// Verify concurrent reservations, private row isolation, undo, independent object/generation namespaces and full-width exhaustion. Reopening the same owner `[11; 16]` / generation `[12; 16]` must allocate above the returned watermark.
pub fn verify_document_id_sessions(
    a: &PersistentStorageSession,
    b: &PersistentStorageSession,
) -> StorageBackendResult<u64> {
    let a_ids = DocumentIdAllocator::new(a.backend.identifier_allocator(), [11; 16], [12; 16])?;
    let b_ids = DocumentIdAllocator::new(b.backend.identifier_allocator(), [11; 16], [12; 16])?;
    assert!(a_ids.is_durable());
    assert!(b_ids.is_durable());
    let mut seed = 101;
    a_ids.synchronize(&mut seed)?;
    a.backend.begin_read_transaction()?;
    assert!(a.catalog.get_metadata("document_ids_private")?.is_none());
    a.catalog.set_metadata("document_ids_private", "private")?;
    let checkpoint = StorageSavepointId::allocate();
    a.backend.savepoint(checkpoint)?;
    let barrier = std::sync::Barrier::new(2);
    let (a_id, b_id) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            a_ids.allocate(&mut 1)
        });
        let second = scope.spawn(|| {
            barrier.wait();
            b_ids.allocate(&mut 1)
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    let mut issued = [a_id?, b_id?];
    issued.sort_unstable();
    assert_eq!(issued, [101, 102]);
    assert!(b.catalog.get_metadata("document_ids_private")?.is_none());
    b.catalog
        .set_metadata("document_ids_committed", "committed")?;
    assert!(a.backend.in_transaction());
    a.backend.rollback_to_savepoint(checkpoint)?;
    assert_eq!(a_ids.allocate(&mut 1)?, 103);
    a.backend.rollback_transaction()?;
    assert!(a.catalog.get_metadata("document_ids_private")?.is_none());
    assert_eq!(
        a.catalog.get_metadata("document_ids_committed")?.as_deref(),
        Some("committed")
    );
    let last = a_ids.allocate(&mut 1)?;
    assert_eq!(last, 104);
    for (object, generation) in [([13; 16], [12; 16]), ([11; 16], [13; 16])] {
        let other = DocumentIdAllocator::new(a.backend.identifier_allocator(), object, generation)?;
        assert_eq!(other.allocate(&mut 1)?, 1);
    }
    let full = DocumentIdAllocator::new(a.backend.identifier_allocator(), [11; 16], [14; 16])?;
    let mut floor = u128::from(u64::MAX);
    full.synchronize(&mut floor)?;
    assert_eq!(full.allocate(&mut 1)?, u64::MAX);
    let mut stale = 1;
    assert!(full.allocate(&mut stale).is_err());
    assert_eq!(stale, 1);
    full.observe(&mut stale, 1)?;
    assert_eq!(stale, u128::from(u64::MAX) + 1);
    for (object, generation) in [([0; 16], [1; 16]), ([1; 16], [0; 16])] {
        assert!(
            DocumentIdAllocator::new(a.backend.identifier_allocator(), object, generation).is_err()
        );
    }
    Ok(last)
}
