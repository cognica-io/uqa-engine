//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-shaped logical roles and routine execution contexts.

use std::collections::BTreeMap;
use uqa_sql::catalog::roles::RoleReference;

use uqa_sql::catalog::roles::{memberships::role_is_superuser, session::SessionAuthorization};
use uqa_sql::SQLError;

use crate::{Engine, SQLStatementCache, StorageBackendResult};
use uqa_execution::catalog::security::roles::persistence as role_catalog;

pub(crate) struct RoutineSessionStateGuard<'a> {
    engine: &'a Engine,
    search_path: Vec<String>,
    session_vars: BTreeMap<String, String>,
    sql_statement_cache: Option<SQLStatementCache>,
    authorization: Option<SessionAuthorization>,
}

pub(crate) use uqa_execution::routines::invocation::scopes::active_routine_reads_command_overlay;

impl RoutineSessionStateGuard<'_> {
    fn capture(engine: &Engine, preserve_statement_cache: bool) -> RoutineSessionStateGuard<'_> {
        let state = engine.session.state.read();
        RoutineSessionStateGuard {
            engine,
            search_path: state.search_path.clone(),
            session_vars: state.session_vars.clone(),
            sql_statement_cache: preserve_statement_cache
                .then(|| state.sql_statement_cache.clone()),
            authorization: Some(state.authorization.clone()),
        }
    }

    pub(crate) fn preserve_authorization(&mut self) {
        self.authorization = None;
    }
}

impl Drop for RoutineSessionStateGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.engine.session.state.write();
        state.search_path = std::mem::take(&mut self.search_path);
        state.session_vars = std::mem::take(&mut self.session_vars);
        if let Some(cache) = self.sql_statement_cache.take() {
            state.sql_statement_cache = cache;
        }
        if let Some(authorization) = self.authorization.take() {
            state.authorization = authorization;
        }
    }
}

pub(crate) use uqa_sql::catalog::roles::{RoleDefinition, RoleMembership, RoleMembershipKey};

impl Engine {
    pub(crate) fn current_role(&self) -> RoleReference {
        self.session_execution_view().current_role()
    }

    pub(crate) fn session_role(&self) -> RoleReference {
        self.session_execution_view().session_role()
    }

    pub(crate) fn current_user_is_superuser(&self) -> bool {
        let current = self.current_role();
        role_is_superuser(&self.durable.roles.read(), &current)
    }

    pub(crate) fn current_user_has_role_privileges(&self, target: &str) -> bool {
        let current = self.current_role();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        role_inherits(&roles, &memberships, &current, target)
    }

    pub(crate) fn persist_roles_snapshot(
        &self,
        before: &BTreeMap<String, RoleDefinition>,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<(), SQLError> {
        role_catalog::persist_roles(self.storage.catalog.as_deref(), before, roles)
    }

    pub(crate) fn persist_role_memberships_snapshot(
        &self,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        role_catalog::persist_memberships(self.storage.catalog.as_deref(), memberships)
    }

    pub(crate) fn restore_roles_from_metadata(
        &self,
        catalog: &dyn crate::CatalogFacade,
        allow_migration: bool,
    ) -> StorageBackendResult<()> {
        let values = if allow_migration {
            role_catalog::restore_and_migrate(catalog)?
        } else {
            role_catalog::restore(catalog)?
        };
        *self.durable.roles.write() = values.roles;
        *self.durable.role_memberships.write() = values.memberships;
        Ok(())
    }

    pub(crate) fn with_current_user_context<T>(
        &self,
        current_user: &str,
        execute: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let _guard = self.routine_session_state_guard();
        let role = RoleReference::from(current_user).bind(&self.durable.roles.read())?;
        self.session
            .state
            .write()
            .authorization
            .set_effective(role.into());
        execute()
    }

    pub(crate) fn routine_session_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, false)
    }

    pub(crate) fn routine_config_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, true)
    }
}

pub(crate) use uqa_sql::catalog::roles::role_inherits;
