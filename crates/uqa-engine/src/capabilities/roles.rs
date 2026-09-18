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
use uqa_sql::catalog::roles::identity::RoleBinding;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    catalog::{
        roles::{
            definition::{RoleNotices, RoleValidationContext},
            dependencies::context::{
                RoleDependencyCatalog, RoleDependencyRead, RoleTableSecurity, RoleTablesRead,
            },
            RoleDefinition, RoleMembership, RoleMembershipKey,
        },
        security::{
            database::BoundDatabaseSecurity, BoundSchemaSecurity, BoundTableSecurity,
            SequenceSecurity,
        },
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
    fn inquiry_role_definitions(
        &self,
    ) -> Result<uqa_execution::catalog::security::roles::RoleDefinitionRead<'_>, SQLError> {
        use uqa_execution::catalog::security::roles::{
            persistence::RoleCatalogSnapshot, snapshot::read_role_snapshot,
        };
        let session = self
            .open_independent_catalog_session()
            .map_err(|error| SQLError::Internal(format!("open role inquiry snapshot: {error}")))?;
        let snapshot = read_role_snapshot(
            RoleCatalogSnapshot {
                roles: self.durable.roles.snapshot(),
                memberships: self.durable.role_memberships.snapshot(),
            },
            self.storage.catalog.as_deref(),
            session.as_ref(),
            self.versioned_backend_transactions(),
        )
        .map_err(|error| SQLError::Internal(format!("load role inquiry snapshot: {error}")))?;
        Ok(Box::new(snapshot.roles))
    }
}
impl uqa_sql::catalog::roles::RoleReferenceNames for Engine {
    fn current_role(&self) -> RoleReference {
        Engine::current_role(self)
    }
    fn session_role(&self) -> RoleReference {
        Engine::session_role(self)
    }
    fn authenticated_role(&self) -> RoleReference {
        RoleReference::Bound(
            self.session
                .state
                .read()
                .authorization
                .authenticated()
                .clone(),
        )
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
            locks: self,
            temporary_roles: self,
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
    fn persist_roles(
        &self,
        before: &BTreeMap<String, RoleDefinition>,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<(), SQLError> {
        self.persist_roles_snapshot(before, roles)
    }
    fn persist_memberships(
        &self,
        before: &BTreeMap<RoleMembershipKey, RoleMembership>,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        self.persist_role_memberships_snapshot(before, memberships)
    }
    fn catalog_changed(&self) {
        self.note_catalog_registry_changed();
    }
    fn set_current_role(&self, target: Option<RoleBinding>) {
        let mut state = self.session.state.write();
        state.authorization.set_role(target.map(Arc::new));
        state.sql_statement_cache.clear();
    }
    fn set_session_authorization(&self, target: RoleBinding) {
        let mut state = self.session.state.write();
        state.authorization.set_session(Arc::new(target));
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
    fn persistence(&self) -> uqa_sql::ast::RelationPersistence {
        self.persistence
    }
    fn security(&self) -> BoundTableSecurity {
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
    fn database(&self) -> RoleDependencyRead<'_, BoundDatabaseSecurity> {
        Box::new(self.durable.database_security.read())
    }
    fn schemas(&self) -> RoleDependencyRead<'_, BTreeMap<String, BoundSchemaSecurity>> {
        Box::new(self.durable.schemas.read())
    }
    fn tables(&self) -> Box<dyn RoleTablesRead + '_> {
        Box::new(RoleTableRegistryRead(self.storage.tables.read()))
    }
    fn views(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, StoredView>> {
        Box::new(self.durable.views.read())
    }
    fn foreign_tables(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, BoundTableSecurity>> {
        Box::new(self.durable.foreign_table_security.read())
    }
    fn sequences(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, SequenceSecurity>> {
        Box::new(self.durable.sequence_security.read())
    }
    fn routines(&self) -> RoleDependencyRead<'_, BTreeMap<String, Vec<Arc<SQLUserFunction>>>> {
        Box::new(self.durable.sql_user_functions.read())
    }
}

impl uqa_sql::catalog::roles::dependencies::context::TemporaryRoleDependencyCatalog for Engine {
    fn temporary_namespace_allocated(&self) -> bool {
        self.session.state.read().temporary_namespace_allocated
    }
    fn sequence_persistence(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, uqa_sql::ast::RelationPersistence>> {
        Box::new(self.durable.sequence_persistence.read())
    }
}

impl uqa_execution::catalog::security::roles::temporary::TemporaryRoleDependencyReads for Engine {
    fn peer_temporary_role_reference(&self, oid: u32) -> Result<bool, SQLError> {
        self.row_locks.peer_temporary_role_reference(
            self.session_id,
            oid,
            &self.runtime.cancellation,
        )
    }
}

impl Engine {
    pub(crate) fn prepare_temporary_role_publication(
        &self,
    ) -> Result<
        Option<uqa_execution::row_locks::temporary_roles::TemporaryRolePublication<'_>>,
        SQLError,
    > {
        uqa_execution::catalog::security::roles::temporary::prepare_temporary_roles(
            uqa_execution::catalog::security::roles::temporary::TemporaryRoleContext {
                catalog: self,
                roles: self,
                manager: &self.row_locks,
                session: self.session_id,
                cancellation: &self.runtime.cancellation,
            },
        )
    }
}
