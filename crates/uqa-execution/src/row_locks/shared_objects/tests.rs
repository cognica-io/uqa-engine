//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::RowLockManager;

#[test]
fn shared_object_lock_addresses_are_typed_stable_and_savepoint_owned() {
    let manager = RowLockManager::new();
    let peer = RowLockManager::new();
    let object = SharedCatalogLock::Object {
        class_id: 1260,
        oid: 20_001,
    };
    let key = manager.shared_catalog_key(object);
    peer.table_key("registered first");
    assert_eq!(
        manager.relation_bytes(key),
        peer.relation_bytes(peer.shared_catalog_key(object))
    );
    for target in [
        SharedCatalogLock::Object {
            class_id: 1261,
            oid: 20_001,
        },
        SharedCatalogLock::Object {
            class_id: 1260,
            oid: 20_002,
        },
        SharedCatalogLock::Name {
            class_id: 1260,
            name: "20001",
        },
        SharedCatalogLock::Name {
            class_id: 1260,
            name: "1260:20001",
        },
    ] {
        assert_ne!(
            manager.relation_bytes(key),
            manager.relation_bytes(manager.shared_catalog_key(target))
        );
    }
    assert_ne!(
        manager.relation_bytes(key),
        manager.relation_bytes(manager.table_key("1260:20001"))
    );
    let cancel = uqa_core::CancellationToken::new();
    manager
        .acquire_scoped_relation(1, key, RelationLockMode::AccessShare, (3, 4), &cancel)
        .unwrap()
        .retain();
    assert!(!manager
        .try_acquire_relation(2, key, RelationLockMode::AccessExclusive, 0, &cancel)
        .unwrap());
    manager.release_mark_above(1, 3);
    assert!(!manager
        .try_acquire_relation(2, key, RelationLockMode::AccessExclusive, 0, &cancel)
        .unwrap());
    manager.release_mark_above(1, 2);
    assert!(manager
        .try_acquire_relation(2, key, RelationLockMode::AccessExclusive, 0, &cancel)
        .unwrap());
}
