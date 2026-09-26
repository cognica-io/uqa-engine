//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    expect, expect_eq, Arc, KeyValueDiskANNStore, KeyValueStore, Keys, StorageBackendResult,
    StorageReadControl,
};

pub(super) fn verify(
    store: &Arc<dyn KeyValueStore>,
    repository: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let allocator = store.identifier_allocator().expect("versioned identifiers");
    let mut previous = allocator
        .identifier_watermark(super::super::identifiers::NAMESPACE)?
        .unwrap_or(0);
    for (table, index) in [(501, 501), (501, 502), (502, 501), (501, 501)] {
        let stage = repository.allocate_stage(table, index, control)?;
        let generation = stage.generation();
        expect(
            generation.generation() > previous,
            "independent indexes share non-reused generation reservations",
        )?;
        expect_eq(
            &allocator.identifier_watermark(Keys::new(generation).legacy_allocation_namespace())?,
            &None,
            "new indexes create no per-index allocation watermark",
        )?;
        previous = generation.generation();
    }
    expect_eq(
        &allocator.identifier_watermark(super::super::identifiers::NAMESPACE)?,
        &Some(previous),
        "all generation reservations share one durable watermark",
    )
}
