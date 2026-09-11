//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind role lifecycle consumers to live catalog guards, provider writes and session state.

use crate::{Engine, TableState};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::security::role_lifecycle::context::{
    RoleDefinitionWrite, RoleExecutionContext, RoleMembershipWrite, RolePublication, RoleRegistry,
};
use uqa_sql::{
    catalog::{
        roles::{
            definition::{RoleNotices, RoleValidationContext},
            dependencies::context::{
                RoleDependencyCatalog, RoleDependencyRead, RoleTableSecurity, RoleTablesRead,
            },
            RoleDefinition, RoleMembership, RoleMembershipKey,
        },
        security::{database::DatabaseSecurity, SchemaSecurity, SequenceSecurity, TableSecurity},
        stored_view::StoredView,
    },
    routines::SQLUserFunction,
    SQLError,
};
impl uqa_execution::catalog::security::roles::RoleCatalogGuards for Engine {
    fn role_definitions(&self) -> uqa_execution::catalog::security::roles::RoleDefinitionRead<'_> {
        Box::new(self.durable.roles.read())
    }
    fn role_memberships(&self) -> uqa_execution::catalog::security::roles::RoleMembershipRead<'_> {
        Box::new(self.durable.role_memberships.read())
    }
}
impl uqa_sql::catalog::roles::RoleReferenceNames for Engine {
    fn current_user_name(&self) -> String {
        Engine::current_user_name(self)
    }
    fn session_user_name(&self) -> String {
        Engine::session_user_name(self)
    }
}

impl Engine {
    pub(crate) fn role_execution_context(&self) -> RoleExecutionContext<'_> {
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
}
impl RoleRegistry for Engine {
    fn write_roles(&self) -> RoleDefinitionWrite<'_> {
        Box::new(self.durable.roles.write())
    }
    fn write_memberships(&self) -> RoleMembershipWrite<'_> {
        Box::new(self.durable.role_memberships.write())
    }
}
impl RolePublication for Engine {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
    fn persist_roles(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<(), SQLError> {
        self.persist_roles_snapshot(roles)
    }
    fn persist_memberships(
        &self,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        self.persist_role_memberships_snapshot(memberships)
    }
    fn catalog_changed(&self) {
        self.note_catalog_registry_changed();
    }
    fn set_current_role(&self, target: String) {
        let mut state = self.session.state.write();
        state.current_user = target;
        state.sql_statement_cache.clear();
    }
}
impl RoleNotices for Engine {
    fn notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
struct RoleTableRegistryRead<'a>(
    parking_lot::RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>,
);
impl RoleTableSecurity for TableState {
    fn security(&self) -> TableSecurity {
        TableState::security(self)
    }
}
impl RoleTablesRead for RoleTableRegistryRead<'_> {
    fn iter(&self) -> Box<dyn Iterator<Item = (&RelationIdentity, &dyn RoleTableSecurity)> + '_> {
        Box::new(
            self.0
                .iter()
                .map(|(relation, table)| (relation, table.as_ref() as &dyn RoleTableSecurity)),
        )
    }
}
impl RoleDependencyCatalog for Engine {
    fn database(&self) -> RoleDependencyRead<'_, DatabaseSecurity> {
        Box::new(self.durable.database_security.read())
    }
    fn schemas(&self) -> RoleDependencyRead<'_, BTreeMap<String, SchemaSecurity>> {
        Box::new(self.durable.schemas.read())
    }
    fn tables(&self) -> Box<dyn RoleTablesRead + '_> {
        Box::new(RoleTableRegistryRead(self.storage.tables.read()))
    }
    fn views(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, StoredView>> {
        Box::new(self.durable.views.read())
    }
    fn foreign_tables(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, TableSecurity>> {
        Box::new(self.durable.foreign_table_security.read())
    }
    fn sequences(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, SequenceSecurity>> {
        Box::new(self.durable.sequence_security.read())
    }
    fn routines(&self) -> RoleDependencyRead<'_, BTreeMap<String, Vec<Arc<SQLUserFunction>>>> {
        Box::new(self.durable.sql_user_functions.read())
    }
}
