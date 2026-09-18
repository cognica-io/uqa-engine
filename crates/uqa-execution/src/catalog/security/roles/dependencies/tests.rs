//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    RelationLockMode, RowLockManager, ScopedRelationLock,
};
use std::{
    cell::{Cell, RefCell, RefMut},
    collections::BTreeMap,
};
use uqa_sql::catalog::roles::{
    guards::RoleCatalogGuards, RoleDefinition, RoleMembership, RoleMembershipKey,
};

#[derive(Default)]
struct Catalog {
    roles: RefCell<BTreeMap<String, RoleDefinition>>,
    memberships: RefCell<BTreeMap<RoleMembershipKey, RoleMembership>>,
    object: RefCell<()>,
    locks: RowLockManager,
    writer: Cell<usize>,
    acquired: Cell<usize>,
    replace: Cell<bool>,
}

impl Catalog {
    fn new() -> Self {
        let catalog = Self::default();
        for (index, name) in ["reader", "grantor"].into_iter().enumerate() {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_000 + i64::try_from(index).unwrap();
            role.object_id = [u8::try_from(index + 1).unwrap(); 16];
            catalog.roles.borrow_mut().insert(name.into(), role);
        }
        catalog
    }

    fn assert_released(&self) {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
        assert!(self.object.try_borrow_mut().is_ok());
    }

    fn candidate(&self) -> RoleDependencyCandidate<'_, RefMut<'_, ()>> {
        let roles = self.role_definitions();
        let memberships = self.role_memberships();
        let mut dependencies = BTreeSet::from(["reader".into()]);
        if self.writer.get() > 0 {
            dependencies.insert("grantor".into());
        }
        RoleDependencyCandidate {
            value: self.object.borrow_mut(),
            memberships,
            roles,
            dependencies,
        }
    }
}

impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        assert!(self.memberships.try_borrow_mut().is_ok());
        Box::new(self.roles.borrow())
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        assert!(self.roles.try_borrow_mut().is_err());
        Box::new(self.memberships.borrow())
    }
}

impl SharedObjectLockSession for Catalog {
    fn acquire_shared_catalog(
        &self,
        target: SharedCatalogLock<'_>,
        mode: RelationLockMode,
    ) -> Result<ScopedRelationLock<'_>, SQLError> {
        self.assert_released();
        assert_eq!(mode, RelationLockMode::AccessShare);
        self.acquired.set(self.acquired.get() + 1);
        self.locks.acquire_scoped_relation(
            1,
            self.locks.shared_catalog_key(target),
            mode,
            (0, 1),
            &uqa_core::CancellationToken::new(),
        )
    }
    fn refresh_shared_catalog(&self) -> Result<(), SQLError> {
        self.assert_released();
        if self.replace.get() {
            self.roles.borrow_mut().get_mut("reader").unwrap().object_id = [9; 16];
        }
        Ok(())
    }
}

#[test]
fn added_dependencies_after_writer_preparation_release_guards_and_prepare_again() {
    let catalog = Catalog::new();
    let result = prepare_role_dependencies(
        &RoleLockContext {
            roles: &catalog,
            session: &catalog,
        },
        || {
            catalog.assert_released();
            catalog.writer.set(catalog.writer.get() + 1);
            Ok(())
        },
        || Ok(catalog.candidate()),
    )
    .unwrap();
    assert_eq!(catalog.acquired.get(), 2);
    assert_eq!(catalog.writer.get(), 2);
    assert!(catalog.roles.try_borrow_mut().is_err());
    assert!(catalog.memberships.try_borrow_mut().is_err());
    assert!(catalog.object.try_borrow_mut().is_err());
    drop(result);
    catalog.assert_released();
}

#[test]
fn preparation_rejects_the_original_role_replaced_during_refresh_before_writer_admission() {
    let catalog = Catalog::new();
    catalog.replace.set(true);
    let result = prepare_role_dependencies(
        &RoleLockContext {
            roles: &catalog,
            session: &catalog,
        },
        || panic!("a replaced role must fail before writer admission"),
        || Ok(catalog.candidate()),
    );
    assert_eq!(result.err().unwrap().sqlstate(), Some("42704"));
    catalog.assert_released();
    let key = catalog.locks.shared_catalog_key(SharedCatalogLock::Object {
        class_id: super::super::locking::ROLE_CATALOG_CLASS_ID,
        oid: 20_000,
    });
    assert!(catalog
        .locks
        .try_acquire_relation(
            2,
            key,
            RelationLockMode::AccessExclusive,
            0,
            &uqa_core::CancellationToken::new(),
        )
        .unwrap());
}

#[test]
fn owner_binding_survives_target_and_writer_waits_without_rebinding_the_name() {
    for replace_before_preflight in [false, true] {
        let catalog = Catalog::new();
        let context = RoleLockContext {
            roles: &catalog,
            session: &catalog,
        };
        let owner = context.bind(&"reader".into()).unwrap();
        if replace_before_preflight {
            catalog
                .roles
                .borrow_mut()
                .get_mut("reader")
                .unwrap()
                .object_id = [8; 16];
        }
        let result = prepare_role_owner(
            context,
            &owner,
            || {
                catalog.assert_released();
                catalog
                    .roles
                    .borrow_mut()
                    .get_mut("reader")
                    .unwrap()
                    .object_id = [9; 16];
                Ok(())
            },
            |_, _| {
                assert!(
                    !replace_before_preflight,
                    "replacement is rejected before authorization"
                );
                Ok(Some(catalog.object.borrow_mut()))
            },
        );
        assert_eq!(result.err().unwrap().sqlstate(), Some("42704"));
        catalog.assert_released();
        assert_eq!(
            catalog.acquired.get(),
            usize::from(!replace_before_preflight)
        );
    }
}

#[test]
fn unchanged_owner_skips_dependency_locks_and_writer_admission() {
    let catalog = Catalog::new();
    let context = RoleLockContext {
        roles: &catalog,
        session: &catalog,
    };
    let owner = context.bind(&"reader".into()).unwrap();
    let result = prepare_role_owner(
        context,
        &owner,
        || panic!("an unchanged owner does not admit a writer"),
        |_, _| Ok(None::<()>),
    )
    .unwrap();
    assert!(result.value.is_none());
    assert_eq!(catalog.acquired.get(), 0);
    drop(result);
    catalog.assert_released();
}
