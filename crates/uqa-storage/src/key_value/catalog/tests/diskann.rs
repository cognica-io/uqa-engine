//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::format::{
    DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion,
};
use crate::key_value::codec::vector_key;
use crate::key_value::vector_index::origin::{self, journal};
use crate::mvcc::{DatabaseId, StorageTransactionId};

#[test]
fn diskann_changes_and_origins_follow_legacy_relation_migration_without_rewriting_versions() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    store
        .put(
            &single_str_key(TAG_TABLE, "legacy_changes").unwrap(),
            &legacy_table_value("legacy_changes"),
        )
        .unwrap();
    let writer = StorageTransactionId::new(DatabaseId::from_bytes([3; 16]), 17).unwrap();
    let version = DiskANNVectorVersion::new(writer, 9).unwrap();
    let record = DiskANNCanonicalOrigin::new(version, 2, 1).unwrap().encode();
    let identity = DiskANNChangeIdentity::new(4, version);
    let origin_key = |table| {
        let mut key = origin::prefix(table, "embedding").unwrap();
        key.extend_from_slice(&4_u64.to_be_bytes());
        key
    };
    let vector = [1.0_f32.to_le_bytes(), (-0.0_f32).to_le_bytes()].concat();
    store.put(&origin_key("legacy_changes"), &record).unwrap();
    store
        .put(
            &journal::key("legacy_changes", "embedding", identity).unwrap(),
            &record,
        )
        .unwrap();
    store
        .put(
            &vector_key("legacy_changes", "embedding", 4, 0).unwrap(),
            &vector,
        )
        .unwrap();
    catalog.migrate_relation_namespace().unwrap();
    catalog.migrate_relation_namespace().unwrap();
    for (before, after, expected) in [
        (
            origin_key("legacy_changes"),
            origin_key("public.legacy_changes"),
            record.to_vec(),
        ),
        (
            journal::key("legacy_changes", "embedding", identity).unwrap(),
            journal::key("public.legacy_changes", "embedding", identity).unwrap(),
            record.to_vec(),
        ),
        (
            vector_key("legacy_changes", "embedding", 4, 0).unwrap(),
            vector_key("public.legacy_changes", "embedding", 4, 0).unwrap(),
            vector,
        ),
    ] {
        assert!(store.get(&before).unwrap().is_none());
        assert_eq!(store.get(&after).unwrap(), Some(expected));
    }
}
