//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::context::{
    RoleDefinitionWrite, RoleMembershipWrite, RolePublication, RoleRegistry,
};
use super::*;
use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RoleAttribute,
    catalog::{
        roles::{
            definition::{RoleNotices, RoleValidationContext},
            dependencies::context::{
                RoleDependencyCatalog, RoleDependencyRead, RoleTableSecurity, RoleTablesRead,
            },
            guards::{RoleCatalogGuards, RoleDefinitionRead, RoleMembershipRead},
            RoleDefinition, RoleMembership, RoleMembershipKey, RoleReferenceNames,
        },
        security::{database::DatabaseSecurity, SchemaSecurity, SequenceSecurity, TableSecurity},
        stored_view::StoredView,
    },
    routines::SQLUserFunction,
};

pub(super) struct Catalog {
    pub roles: RefCell<BTreeMap<String, RoleDefinition>>,
    pub memberships: RefCell<BTreeMap<RoleMembershipKey, RoleMembership>>,
    pub current: RefCell<String>,
    pub events: RefCell<Vec<String>>,
    pub fail_membership_persistence: Cell<bool>,
    pub epoch: Cell<usize>,
    database: DatabaseSecurity,
    schemas: BTreeMap<String, SchemaSecurity>,
    views: BTreeMap<RelationIdentity, StoredView>,
    foreign_tables: BTreeMap<RelationIdentity, TableSecurity>,
    sequences: BTreeMap<RelationIdentity, SequenceSecurity>,
    routines: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
}
impl Catalog {
    pub fn new() -> Self {
        Self {
            roles: RefCell::new(BTreeMap::from([(
                "uqa".into(),
                RoleDefinition::bootstrap(),
            )])),
            memberships: RefCell::new(BTreeMap::new()),
            current: RefCell::new("uqa".into()),
            events: RefCell::new(Vec::new()),
            fail_membership_persistence: Cell::new(false),
            epoch: Cell::new(0),
            database: DatabaseSecurity::bootstrap(),
            schemas: BTreeMap::new(),
            views: BTreeMap::new(),
            foreign_tables: BTreeMap::new(),
            sequences: BTreeMap::new(),
            routines: BTreeMap::new(),
        }
    }
    pub fn context(&self) -> RoleExecutionContext<'_> {
        RoleExecutionContext {
            analysis: RoleValidationContext {
                names: self,
                roles: self,
                notices: self,
            },
            registry: self,
            publication: self,
            dependencies: self,
        }
    }
    pub fn role(&self, name: &str, attributes: &[RoleAttribute]) {
        let mut statement = create(name);
        statement.attributes = attributes.iter().copied().collect();
        self.roles
            .borrow_mut()
            .insert(name.into(), RoleDefinition::from_create(&statement));
    }
    pub fn membership(&self, role: &str, member: &str, grantor: &str) {
        let membership = RoleMembership {
            oid: 31,
            role: role.into(),
            member: member.into(),
            grantor: grantor.into(),
            admin_option: true,
            inherit_option: false,
            set_option: false,
        };
        self.memberships
            .borrow_mut()
            .insert(membership.key(), membership);
    }
    pub fn released(&self) {
        assert!(self.roles.try_borrow_mut().is_ok());
        assert!(self.memberships.try_borrow_mut().is_ok());
    }
    fn event(&self, value: &str) {
        self.events.borrow_mut().push(value.into());
    }
}
pub(super) fn create(name: &str) -> CreateRoleStmt {
    CreateRoleStmt {
        name: name.into(),
        attributes: BTreeSet::from([RoleAttribute::Inherit]),
        connection_limit: -1,
        in_roles: Vec::new(),
        role_members: Vec::new(),
        admin_members: Vec::new(),
    }
}
struct Read<'a, T> {
    value: Ref<'a, T>,
    catalog: &'a Catalog,
    name: &'static str,
}
impl<T> Deref for Read<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> Drop for Read<'_, T> {
    fn drop(&mut self) {
        self.catalog.event(&format!("release {}", self.name));
    }
}
struct Write<'a, T> {
    value: RefMut<'a, T>,
    catalog: &'a Catalog,
    name: &'static str,
}
impl<T> Deref for Write<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> DerefMut for Write<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.catalog.event(&format!("publish {}", self.name));
        &mut self.value
    }
}
impl<T> Drop for Write<'_, T> {
    fn drop(&mut self) {
        self.catalog.event(&format!("release {}", self.name));
    }
}
impl RoleReferenceNames for Catalog {
    fn current_user_name(&self) -> String {
        self.event("current");
        self.current.borrow().clone()
    }
    fn session_user_name(&self) -> String {
        self.event("session");
        "uqa".into()
    }
}
impl RoleCatalogGuards for Catalog {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        self.event("read roles");
        Box::new(Read {
            value: self.roles.borrow(),
            catalog: self,
            name: "roles",
        })
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        self.event("read memberships");
        Box::new(Read {
            value: self.memberships.borrow(),
            catalog: self,
            name: "memberships",
        })
    }
}
impl RoleRegistry for Catalog {
    fn write_roles(&self) -> RoleDefinitionWrite<'_> {
        self.event("write roles");
        Box::new(Write {
            value: self.roles.borrow_mut(),
            catalog: self,
            name: "roles",
        })
    }
    fn write_memberships(&self) -> RoleMembershipWrite<'_> {
        self.event("write memberships");
        Box::new(Write {
            value: self.memberships.borrow_mut(),
            catalog: self,
            name: "memberships",
        })
    }
}
impl RolePublication for Catalog {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.event("writer");
        Ok(())
    }
    fn persist_roles(&self, _: &BTreeMap<String, RoleDefinition>) -> Result<(), SQLError> {
        assert!(self.roles.try_borrow_mut().is_err());
        self.event("persist roles");
        Ok(())
    }
    fn persist_memberships(
        &self,
        _: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        assert!(self.roles.try_borrow_mut().is_err());
        assert!(self.memberships.try_borrow_mut().is_err());
        self.event("persist memberships");
        if self.fail_membership_persistence.get() {
            return Err(SQLError::Internal("membership write failed".into()));
        }
        Ok(())
    }
    fn catalog_changed(&self) {
        self.released();
        self.event("epoch");
        self.epoch.set(self.epoch.get() + 1);
    }
    fn set_current_role(&self, target: String) {
        self.released();
        self.event("set current");
        *self.current.borrow_mut() = target;
    }
}
impl RoleNotices for Catalog {
    fn notice(&self, level: &str, message: &str) {
        assert!(self.roles.try_borrow().is_err());
        self.event(&format!("{level}: {message}"));
    }
}
struct EmptyTables;
impl RoleTablesRead for EmptyTables {
    fn iter(&self) -> Box<dyn Iterator<Item = (&RelationIdentity, &dyn RoleTableSecurity)> + '_> {
        Box::new(std::iter::empty())
    }
}
impl RoleDependencyCatalog for Catalog {
    fn database(&self) -> RoleDependencyRead<'_, DatabaseSecurity> {
        self.event("database");
        Box::new(&self.database)
    }
    fn schemas(&self) -> RoleDependencyRead<'_, BTreeMap<String, SchemaSecurity>> {
        self.event("schemas");
        Box::new(&self.schemas)
    }
    fn tables(&self) -> Box<dyn RoleTablesRead + '_> {
        self.event("tables");
        Box::new(EmptyTables)
    }
    fn views(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, StoredView>> {
        self.event("views");
        Box::new(&self.views)
    }
    fn foreign_tables(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, TableSecurity>> {
        self.event("foreign");
        Box::new(&self.foreign_tables)
    }
    fn sequences(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, SequenceSecurity>> {
        self.event("sequences");
        Box::new(&self.sequences)
    }
    fn routines(&self) -> RoleDependencyRead<'_, BTreeMap<String, Vec<Arc<SQLUserFunction>>>> {
        self.event("routines");
        Box::new(&self.routines)
    }
}
