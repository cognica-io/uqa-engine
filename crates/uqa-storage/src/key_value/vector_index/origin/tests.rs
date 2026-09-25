//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_canonical_origins_reject_unversioned_stores_without_writing() {
    let store = Arc::new(crate::key_value::MemoryKeyValueStore::new());
    assert!(KeyValueDiskANNCanonical::new(store.clone(), "table", "field", 3).is_err());
    let mut invoked = false;
    assert!(store
        .with_versioned_mutation(&mut |_, _, _| {
            invoked = true;
            Ok(())
        })
        .is_err());
    assert!(!invoked);
}
