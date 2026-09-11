//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};
use uqa_sql::{
    ast::{DatabasePrivilege, DatabaseRevokeBehavior},
    catalog::roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition, RoleMembership, RoleMembershipKey,
    },
};

struct DatabaseCatalog {
    security: RefCell<DatabaseSecurity>,
    roles: RefCell<BTreeMap<String, RoleDefinition>>,
    memberships: RefCell<BTreeMap<RoleMembershipKey, RoleMembership>>,
    fail_persistence: Cell<bool>,
    epoch: Cell<usize>,
    publications: RefCell<Vec<&'static str>>,
}

impl DatabaseCatalog {
    fn context(&self) -> DatabasePrivilegeContext<'_> {
        DatabasePrivilegeContext {
            names: self,
            roles: self,
            registry: self,
            publication: self,
        }
    }

    fn assert_authorization_is_retained(&self) {
        assert!(self.roles.try_borrow_mut().is_err());
        assert!(self.memberships.try_borrow_mut().is_err());
    }

    fn assert_guards_are_released(&self) {
        assert!(self.security.try_borrow_mut().is_ok());
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
    }
}

impl RoleReferenceNames for DatabaseCatalog {
    fn current_user_name(&self) -> String {
        "uqa".into()
    }

    fn session_user_name(&self) -> String {
        "uqa".into()
    }
}

impl RoleCatalogGuards for DatabaseCatalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(self.roles.borrow())
    }

    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(self.memberships.borrow())
    }
}

impl DatabaseSecurityRegistry for DatabaseCatalog {
    fn security_read(&self) -> DatabaseSecurityRead<'_> {
        Box::new(self.security.borrow())
    }

    fn security_write(&self) -> DatabaseSecurityWrite<'_> {
        self.assert_authorization_is_retained();
        self.publications.borrow_mut().push("publish");
        Box::new(self.security.borrow_mut())
    }
}

impl DatabasePrivilegePublication for DatabaseCatalog {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        Ok(())
    }

    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn persist_security(&self, security: &DatabaseSecurity) -> Result<(), SQLError> {
        self.assert_authorization_is_retained();
        assert!(self.security.try_borrow_mut().is_ok());
        assert_ne!(*self.security.borrow(), *security);
        self.publications.borrow_mut().push("persist");
        if self.fail_persistence.get() {
            return Err(SQLError::Internal("simulated persistence failure".into()));
        }
        Ok(())
    }

    fn catalog_changed(&self) {
        self.assert_authorization_is_retained();
        assert!(self.security.try_borrow_mut().is_ok());
        self.publications.borrow_mut().push("epoch");
        self.epoch.set(self.epoch.get() + 1);
    }

    fn notice(&self, _: &str, _: &str) {
        self.assert_guards_are_released();
        panic!("the database owner can grant CREATE without a warning");
    }
}

#[test]
fn database_acl_persistence_failure_leaves_security_and_epoch_unchanged() {
    let catalog = DatabaseCatalog {
        security: RefCell::new(DatabaseSecurity::bootstrap()),
        roles: RefCell::new(BTreeMap::from([(
            "uqa".into(),
            RoleDefinition::bootstrap(),
        )])),
        memberships: RefCell::new(BTreeMap::new()),
        fail_persistence: Cell::new(true),
        epoch: Cell::new(0),
        publications: RefCell::new(Vec::new()),
    };
    let statement = GrantDatabaseStmt {
        is_grant: true,
        grant_option: false,
        grant_option_only: false,
        privileges: vec![DatabasePrivilege::Create],
        databases: vec!["uqa".into()],
        grantees: vec!["PUBLIC".into()],
        grantor: None,
        revoke_behavior: DatabaseRevokeBehavior::Restrict,
    };
    let initial = catalog.security.borrow().clone();
    let result = grant_database_privileges(&catalog.context(), &statement);
    assert!(
        matches!(result, Err(SQLError::Internal(message)) if message == "simulated persistence failure")
    );
    assert_eq!(*catalog.security.borrow(), initial);
    assert_eq!(catalog.epoch.get(), 0);
    assert_eq!(*catalog.publications.borrow(), ["persist"]);
    catalog.assert_guards_are_released();

    catalog.fail_persistence.set(false);
    catalog.publications.borrow_mut().clear();
    grant_database_privileges(&catalog.context(), &statement).unwrap();
    assert_ne!(*catalog.security.borrow(), initial);
    assert_eq!(catalog.epoch.get(), 1);
    assert_eq!(
        *catalog.publications.borrow(),
        ["persist", "publish", "epoch"]
    );
    catalog.assert_guards_are_released();
}
