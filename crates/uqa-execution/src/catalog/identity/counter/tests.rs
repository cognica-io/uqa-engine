//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::HashMap;
use uqa_storage::mvcc::IdentifierAllocation;
use uqa_storage::StorageBackendResult;

/// Nontransactional reservations through the same state transitions the storage providers apply.
#[derive(Default)]
struct Reservations {
    watermarks: Mutex<HashMap<Vec<u8>, u64>>,
}

impl IdentifierAllocator for Reservations {
    fn identifier_watermark(&self, namespace: &[u8]) -> StorageBackendResult<Option<u64>> {
        Ok(self.watermarks.lock().get(namespace).copied())
    }

    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: IdentifierRequest,
    ) -> StorageBackendResult<IdentifierAllocation> {
        let mut watermarks = self.watermarks.lock();
        let allocation = request
            .prepare(watermarks.get(namespace).copied())
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        watermarks.insert(namespace.to_vec(), allocation.watermark());
        Ok(allocation)
    }
}

fn next(counter: &CatalogOidCounter, durable: Option<&dyn IdentifierAllocator>) -> u32 {
    counter
        .next_oid(durable, || {
            panic!("a started counter does not inspect the catalog")
        })
        .unwrap()
}

#[test]
fn a_new_database_numbers_user_objects_from_first_normal_object_id() {
    let reservations = Reservations::default();
    for durable in [None, Some(&reservations as &dyn IdentifierAllocator)] {
        let counter = CatalogOidCounter::default();
        assert_eq!(counter.next_oid(durable, || Ok(None)).unwrap(), 16_384);
        assert_eq!(next(&counter, durable), 16_385);
        assert_eq!(next(&counter, durable), 16_386);
    }
}

#[test]
fn a_counter_that_has_not_started_moves_past_the_oids_in_use() {
    let reservations = Reservations::default();
    for durable in [None, Some(&reservations as &dyn IdentifierAllocator)] {
        let counter = CatalogOidCounter::default();
        assert_eq!(
            counter
                .next_oid(durable, || Ok(Some(3_000_000_000)))
                .unwrap(),
            3_000_000_001
        );
        assert_eq!(next(&counter, durable), 3_000_000_002);
    }
}

#[test]
fn durable_positions_are_shared_and_survive_the_process_counter() {
    let reservations = Reservations::default();
    let first = CatalogOidCounter::default();
    assert_eq!(
        first.next_oid(Some(&reservations), || Ok(None)).unwrap(),
        16_384
    );
    // Another process, or the database reopened, continues the durable sequence without inspecting the catalog.
    let reopened = CatalogOidCounter::default();
    assert_eq!(next(&reopened, Some(&reservations)), 16_385);
    assert_eq!(next(&first, Some(&reservations)), 16_386);
}

#[test]
fn the_sequence_wraps_to_first_normal_object_id_after_the_32_bit_space() {
    assert_eq!(oid_at(u64::from(u32::MAX) - 1), u32::MAX - 1);
    assert_eq!(oid_at(u64::from(u32::MAX)), u32::MAX);
    assert_eq!(oid_at(1_u64 << 32), FIRST_NORMAL_OBJECT_ID);
    assert_eq!(oid_at((1_u64 << 32) + 1), FIRST_NORMAL_OBJECT_ID + 1);
    let reservations = Reservations::default();
    let counter = CatalogOidCounter::default();
    assert_eq!(
        counter
            .next_oid(Some(&reservations), || Ok(Some(u32::MAX - 1)))
            .unwrap(),
        u32::MAX
    );
    assert_eq!(next(&counter, Some(&reservations)), FIRST_NORMAL_OBJECT_ID);
    let process = CatalogOidCounter::default();
    assert_eq!(
        process.next_oid(None, || Ok(Some(u32::MAX))).unwrap(),
        16_384
    );
}
