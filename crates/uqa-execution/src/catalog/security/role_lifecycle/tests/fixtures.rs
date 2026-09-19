//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::context::{
    RoleDefinitionWrite, RoleMembershipWrite, RolePublication, RoleRegistry,
};
use super::*;
use crate::row_locks::shared_objects::SharedCatalogLock;
use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::roles::RoleReference;
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
        security::{
            database::BoundDatabaseSecurity, BoundSchemaSecurity, BoundSequenceSecurity,
            BoundTableSecurity,
        },
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
    pub locks: crate::row_locks::RowLockManager,
    pub cancel: uqa_core::CancellationToken,
    pub refreshed_roles: RefCell<std::collections::VecDeque<BTreeMap<String, RoleDefinition>>>,
    pub refreshed_memberships:
        RefCell<std::collections::VecDeque<BTreeMap<RoleMembershipKey, RoleMembership>>>,
    pub writer_memberships: RefCell<Option<BTreeMap<RoleMembershipKey, RoleMembership>>>,
    pub catalog_locks: RefCell<Vec<(u32, Option<u32>, crate::row_locks::RelationLockMode)>>,
    database: BoundDatabaseSecurity,
    schemas: BTreeMap<String, BoundSchemaSecurity>,
    views: BTreeMap<RelationIdentity, StoredView>,
    foreign_tables: BTreeMap<RelationIdentity, BoundTableSecurity>,
    system_relations: uqa_sql::catalog::security::system_relations::SystemRelationSecurities,
    sequences: BTreeMap<RelationIdentity, BoundSequenceSecurity>,
    routines: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    domains: BTreeMap<String, uqa_sql::catalog::domain::StoredDomain>,
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
            locks: crate::row_locks::RowLockManager::new(),
            cancel: uqa_core::CancellationToken::new(),
            refreshed_roles: RefCell::new(std::collections::VecDeque::new()),
            refreshed_memberships: RefCell::new(std::collections::VecDeque::new()),
            writer_memberships: RefCell::new(None),
            catalog_locks: RefCell::new(Vec::new()),
            database: BoundDatabaseSecurity::bootstrap(),
            schemas: BTreeMap::new(),
            views: BTreeMap::new(),
            foreign_tables: BTreeMap::new(),
            system_relations: BTreeMap::new(),
            sequences: BTreeMap::new(),
            routines: BTreeMap::new(),
            domains: BTreeMap::new(),
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
            locks: self,
            temporary_roles: self,
        }
    }
    pub fn role(&self, name: &str, attributes: &[RoleAttribute]) {
        let mut statement = create(name);
        statement.attributes = attributes.iter().copied().collect();
        let index = self.roles.borrow().len();
        let definition =
            RoleDefinition::from_create(&statement, 20_000 + index as i64, [index as u8; 16]);
        self.roles.borrow_mut().insert(name.into(), definition);
    }
    pub fn membership(&self, role: &str, member: &str, grantor: &str) {
        use uqa_sql::catalog::roles::identity::RoleBinding;
        let roles = self.roles.borrow();
        let membership = RoleMembership {
            oid: 30_000 + self.memberships.borrow().len() as i64,
            role: RoleBinding::from_definition(&roles[role]).unwrap(),
            member: RoleBinding::from_definition(&roles[member]).unwrap(),
            grantor: RoleBinding::from_definition(&roles[grantor]).unwrap(),
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
impl crate::catalog::security::roles::temporary::TemporaryRoleDependencyReads for Catalog {
    fn peer_temporary_role_reference(&self, _: u32) -> Result<bool, SQLError> {
        Ok(false)
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
    fn current_role(&self) -> RoleReference {
        self.event("current");
        self.current.borrow().clone().into()
    }
    fn session_role(&self) -> RoleReference {
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
        self.released();
        self.event("writer");
        if let Some(memberships) = self.writer_memberships.borrow_mut().take() {
            *self.memberships.borrow_mut() = memberships;
        }
        Ok(())
    }
    fn persist_roles(
        &self,
        _: &BTreeMap<String, RoleDefinition>,
        _: &BTreeMap<String, RoleDefinition>,
    ) -> Result<(), SQLError> {
        assert!(self.roles.try_borrow_mut().is_err());
        self.event("persist roles");
        Ok(())
    }
    fn persist_memberships(
        &self,
        _: &BTreeMap<RoleMembershipKey, RoleMembership>,
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
    fn set_current_role(&self, target: Option<uqa_sql::catalog::roles::identity::RoleBinding>) {
        self.released();
        self.event("set current");
        *self.current.borrow_mut() = target.map_or_else(|| "uqa".into(), |role| role.name);
    }
    fn set_session_authorization(&self, target: uqa_sql::catalog::roles::identity::RoleBinding) {
        self.released();
        self.event("set authorization");
        *self.current.borrow_mut() = target.name;
    }
}
impl RoleNotices for Catalog {
    fn notice(&self, level: &str, message: &str) {
        self.released();
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
    fn database(&self) -> RoleDependencyRead<'_, BoundDatabaseSecurity> {
        self.event("database");
        Box::new(&self.database)
    }
    fn schemas(&self) -> RoleDependencyRead<'_, BTreeMap<String, BoundSchemaSecurity>> {
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
    fn foreign_tables(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, BoundTableSecurity>> {
        self.event("foreign");
        Box::new(&self.foreign_tables)
    }
    fn sequences(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, BoundSequenceSecurity>> {
        self.event("sequences");
        Box::new(&self.sequences)
    }
    fn routines(&self) -> RoleDependencyRead<'_, BTreeMap<String, Vec<Arc<SQLUserFunction>>>> {
        self.event("routines");
        Box::new(&self.routines)
    }
    fn domains(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<String, uqa_sql::catalog::domain::StoredDomain>> {
        self.event("domains");
        Box::new(&self.domains)
    }
}

impl uqa_sql::catalog::security::system_relations::SystemRelationSecurityCatalog for Catalog {
    fn system_relation_securities(
        &self,
    ) -> uqa_sql::catalog::security::system_relations::SystemRelationSecurityRead<'_> {
        Box::new(&self.system_relations)
    }
}

impl crate::row_locks::shared_objects::SharedObjectLockSession for Catalog {
    fn acquire_shared_catalog(
        &self,
        target: crate::row_locks::shared_objects::SharedCatalogLock<'_>,
        mode: crate::row_locks::RelationLockMode,
    ) -> Result<crate::row_locks::ScopedRelationLock<'_>, SQLError> {
        self.released();
        self.event("lock role");
        self.catalog_locks.borrow_mut().push(match target {
            SharedCatalogLock::Object { class_id, oid } => (class_id, Some(oid), mode),
            SharedCatalogLock::Name { class_id, .. } => (class_id, None, mode),
        });
        self.locks.acquire_scoped_relation(
            1,
            self.locks.shared_catalog_key(target),
            mode,
            (0, 1),
            &self.cancel,
        )
    }
    fn refresh_shared_catalog(&self) -> Result<(), SQLError> {
        self.released();
        self.event("refresh");
        if let Some(roles) = self.refreshed_roles.borrow_mut().pop_front() {
            *self.roles.borrow_mut() = roles;
        }
        if let Some(memberships) = self.refreshed_memberships.borrow_mut().pop_front() {
            *self.memberships.borrow_mut() = memberships;
        }
        Ok(())
    }
}
