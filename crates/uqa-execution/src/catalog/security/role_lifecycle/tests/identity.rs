//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{create, Catalog};
use crate::{
    catalog::security::{
        role_lifecycle::identity,
        roles::locking::{RoleLockContext, ROLE_CATALOG_CLASS_ID},
    },
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use uqa_sql::catalog::roles::RoleDefinition;

#[test]
fn oid_reservation_retries_a_collision_discovered_after_wait_without_retaining_it() {
    let catalog = Catalog::new();
    let old = catalog.roles.borrow().clone();
    let mut refreshed = old.clone();
    let collision = RoleDefinition::from_create(&create("peer"), 20_001, [1; 16]);
    refreshed.insert(collision.name.clone(), collision);
    catalog.refreshed_roles.borrow_mut().push_back(refreshed);
    let mut candidates = [20_001, 20_002].into_iter();
    let definition = identity::reserve_oid(&catalog.context(), &create("created"), || {
        Ok(candidates.next().unwrap())
    })
    .unwrap();
    assert_eq!(definition.oid, 20_002);
    assert_ne!(definition.object_id, [0; 16]);
    for (oid, available) in [(20_001, true), (20_002, false)] {
        let key = catalog.locks.shared_catalog_key(SharedCatalogLock::Object {
            class_id: ROLE_CATALOG_CLASS_ID,
            oid,
        });
        assert_eq!(
            catalog
                .locks
                .try_acquire_relation(
                    2,
                    key,
                    RelationLockMode::AccessExclusive,
                    0,
                    &catalog.cancel
                )
                .unwrap(),
            available
        );
    }
}

#[test]
fn role_binding_rejects_same_name_and_oid_with_a_different_incarnation() {
    let catalog = Catalog::new();
    catalog.role("original", &[]);
    let context = RoleLockContext {
        roles: &catalog,
        session: &catalog,
    };
    let bound = context.bind("original").unwrap();
    let mut refreshed = catalog.roles.borrow().clone();
    refreshed.get_mut("original").unwrap().object_id = [99; 16];
    catalog.refreshed_roles.borrow_mut().push_back(refreshed);
    let error = context
        .lock(&bound, RelationLockMode::AccessShare)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    let key = catalog.locks.shared_catalog_key(SharedCatalogLock::Object {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: bound.oid,
    });
    assert!(catalog
        .locks
        .try_acquire_relation(
            2,
            key,
            RelationLockMode::AccessExclusive,
            0,
            &catalog.cancel
        )
        .unwrap());
    catalog.released();
}

#[test]
fn competing_name_creation_is_a_unique_violation_after_wait_and_releases_the_name_lock() {
    let catalog = Catalog::new();
    catalog.role("competing", &[]);
    let refreshed = catalog.roles.borrow().clone();
    catalog.roles.borrow_mut().remove("competing");
    catalog.refreshed_roles.borrow_mut().push_back(refreshed);
    let error = identity::reserve_definition(&catalog.context(), &create("competing")).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"));
    let key = catalog.locks.shared_catalog_key(SharedCatalogLock::Name {
        class_id: ROLE_CATALOG_CLASS_ID,
        name: "competing",
    });
    assert!(catalog
        .locks
        .try_acquire_relation(
            2,
            key,
            RelationLockMode::AccessExclusive,
            0,
            &catalog.cancel
        )
        .unwrap());
}
