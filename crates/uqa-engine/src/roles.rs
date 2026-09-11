//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-shaped logical roles and routine execution contexts.

use std::collections::BTreeMap;

use uqa_sql::ast::RoleAttribute;
use uqa_sql::SQLError;

use crate::{
    Engine, SQLStatementCache, StorageBackendError, StorageBackendResult, ROLES_METADATA_KEY,
    ROLE_MEMBERSHIPS_METADATA_KEY,
};

pub(crate) struct RoutineSessionStateGuard<'a> {
    engine: &'a Engine,
    search_path: Vec<String>,
    session_vars: BTreeMap<String, String>,
    sql_statement_cache: Option<SQLStatementCache>,
    current_user: Option<String>,
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
            current_user: Some(state.current_user.clone()),
        }
    }

    pub(crate) fn preserve_current_user(&mut self) {
        self.current_user = None;
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
        if let Some(current_user) = self.current_user.take() {
            state.current_user = current_user;
        }
    }
}

pub(crate) use uqa_sql::catalog::roles::{RoleDefinition, RoleMembership, RoleMembershipKey};

impl Engine {
    pub(crate) fn current_user_name(&self) -> String {
        self.session_execution_view().current_user()
    }

    pub(crate) fn session_user_name(&self) -> String {
        self.session_execution_view().session_user()
    }

    pub(crate) fn current_user_is_superuser(&self) -> bool {
        let current = self.current_user_name();
        self.durable
            .roles
            .read()
            .get(&current)
            .is_some_and(|role| role.has(RoleAttribute::Superuser))
    }

    pub(crate) fn resolve_role_reference(&self, name: &str) -> String {
        uqa_sql::catalog::roles::resolve_role_reference(self, name)
    }

    pub(crate) fn current_user_has_role_privileges(&self, target: &str) -> bool {
        let current = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        role_inherits(&roles, &memberships, &current, target)
    }

    pub(crate) fn persist_roles_snapshot(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let json = serde_json::to_string(roles)
            .map_err(|error| SQLError::Internal(format!("serialize role catalog: {error}")))?;
        catalog
            .set_metadata(ROLES_METADATA_KEY, &json)
            .map_err(|error| SQLError::Internal(format!("persist role catalog: {error}")))
    }

    pub(crate) fn persist_role_memberships_snapshot(
        &self,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let stored = memberships.values().cloned().collect::<Vec<_>>();
        let json = serde_json::to_string(&stored).map_err(|error| {
            SQLError::Internal(format!("serialize role membership catalog: {error}"))
        })?;
        catalog
            .set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, &json)
            .map_err(|error| {
                SQLError::Internal(format!("persist role membership catalog: {error}"))
            })
    }

    pub(crate) fn restore_roles_from_metadata(
        &self,
        catalog: &dyn crate::CatalogFacade,
    ) -> StorageBackendResult<()> {
        let mut roles = match catalog.get_metadata(ROLES_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<BTreeMap<String, RoleDefinition>>(&json)?,
            None => BTreeMap::new(),
        };
        uqa_sql::catalog::roles::restoration::restore_role_definitions(&mut roles)
            .map_err(StorageBackendError::Other)?;
        let memberships = match catalog.get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<Vec<RoleMembership>>(&json)?,
            None => Vec::new(),
        };
        let membership_map =
            uqa_sql::catalog::roles::restoration::restore_role_memberships(&roles, memberships)
                .map_err(StorageBackendError::Other)?;
        *self.durable.roles.write() = roles;
        *self.durable.role_memberships.write() = membership_map;
        Ok(())
    }

    pub(crate) fn with_current_user_context<T>(
        &self,
        current_user: &str,
        execute: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let _guard = self.routine_session_state_guard();
        self.session.state.write().current_user = current_user.to_string();
        execute()
    }

    pub(crate) fn routine_session_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, false)
    }

    pub(crate) fn routine_config_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, true)
    }
}

pub(crate) use uqa_sql::catalog::roles::{role_can_set, role_inherits};

#[cfg(test)]
mod tests;
