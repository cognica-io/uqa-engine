//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::diskann_identifiers::{LEGACY_PREFIX, NAMESPACE};
use uqa_storage::mvcc::IdentifierRequest;

#[test]
fn diskann_generation_allocation_preserves_unmigrated_provider_floors_and_exhaustion() {
    for previous in [700, u64::MAX] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        let database = repository.initialize(&control).unwrap();
        let mut legacy = LEGACY_PREFIX.to_vec();
        legacy.extend_from_slice(&database);
        legacy.extend_from_slice(&11_u64.to_be_bytes());
        legacy.extend_from_slice(&12_u64.to_be_bytes());
        let allocator = store.identifier_allocator().unwrap();
        allocator
            .allocate_identifiers(&legacy, IdentifierRequest::Observe(previous))
            .unwrap();
        let result = repository.allocate_stage(11, 12, &control);
        if previous == u64::MAX {
            assert!(result.is_err());
            assert_eq!(allocator.identifier_watermark(NAMESPACE).unwrap(), None);
        } else {
            let first = result.unwrap();
            assert_eq!(first.generation().generation(), 701);
            let other = repository.allocate_stage(17, 19, &control).unwrap();
            assert_eq!(other.generation().generation(), 702);
            drop((first, other, repository));
            let reopened = KeyValueDiskANNStore::connect(&store, &control).unwrap();
            assert_eq!(
                reopened
                    .allocate_stage(11, 12, &control)
                    .unwrap()
                    .generation()
                    .generation(),
                703
            );
        }
        assert_eq!(
            allocator.identifier_watermark(&legacy).unwrap(),
            Some(previous)
        );
    }
}
