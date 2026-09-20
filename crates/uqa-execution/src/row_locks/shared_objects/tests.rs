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
        SharedCatalogLock::Tuple {
            class_id: 1260,
            oid: 20_001,
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

#[test]
fn member_names_separate_catalogs_owner_classes_incarnations_and_spelling() {
    let manager = RowLockManager::new();
    let peer = RowLockManager::new();
    peer.table_key("unrelated first allocation");
    let mut encodings = std::collections::BTreeSet::new();
    for (class_id, owner_class_id, owner_object_id, name) in [
        (2606, 1259, [1; 16], "same"),
        (2606, 1247, [1; 16], "same"),
        (2606, 1259, [2; 16], "same"),
        (2606, 1259, [1; 16], "other"),
        (2620, 1259, [1; 16], "same"),
    ] {
        let target = SharedCatalogLock::MemberName {
            class_id,
            owner_class_id,
            owner_object_id,
            name,
        };
        let bytes = manager.relation_bytes(manager.shared_catalog_key(target));
        assert_eq!(bytes, peer.relation_bytes(peer.shared_catalog_key(target)));
        assert!(encodings.insert(bytes));
    }
    assert!(
        encodings.insert(manager.relation_bytes(manager.shared_catalog_key(
            SharedCatalogLock::Name {
                class_id: 2606,
                name: "same"
            }
        )))
    );
}
