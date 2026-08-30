//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-shaped logical roles and routine execution contexts.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_sql::ast::{
    AlterRoleStmt, CreateFunction, CreateRoleStmt, DropRoleStmt, FunctionVolatility, RoleAttribute,
};
use uqa_sql::SQLError;

use crate::{
    Engine, SQLStatementCache, StorageBackendError, StorageBackendResult, ROLES_METADATA_KEY,
};

pub(crate) struct RoutineSessionStateGuard<'a> {
    engine: &'a Engine,
    search_path: Vec<String>,
    session_vars: BTreeMap<String, String>,
    sql_statement_cache: Option<SQLStatementCache>,
    current_user: String,
}

thread_local! {
    static ROUTINE_VOLATILITY_STACK: RefCell<Vec<FunctionVolatility>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn active_routine_reads_command_overlay() -> Option<bool> {
    ROUTINE_VOLATILITY_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .map(|volatility| *volatility == FunctionVolatility::Volatile)
    })
}

struct RoutineVolatilityGuard;

impl RoutineVolatilityGuard {
    fn enter(volatility: FunctionVolatility) -> Self {
        ROUTINE_VOLATILITY_STACK.with(|stack| stack.borrow_mut().push(volatility));
        Self
    }
}

impl Drop for RoutineVolatilityGuard {
    fn drop(&mut self) {
        ROUTINE_VOLATILITY_STACK.with(|stack| {
            let removed = stack.borrow_mut().pop();
            debug_assert!(removed.is_some(), "routine volatility stack underflow");
        });
    }
}

impl RoutineSessionStateGuard<'_> {
    fn capture(engine: &Engine, preserve_statement_cache: bool) -> RoutineSessionStateGuard<'_> {
        let state = engine.session.state.read();
        RoutineSessionStateGuard {
            engine,
            search_path: state.search_path.clone(),
            session_vars: state.session_vars.clone(),
            sql_statement_cache: preserve_statement_cache
                .then(|| state.sql_statement_cache.clone()),
            current_user: state.current_user.clone(),
        }
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
        state.current_user = std::mem::take(&mut self.current_user);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoleDefinition {
    pub(crate) oid: i64,
    pub(crate) name: String,
    pub(crate) attributes: BTreeSet<RoleAttribute>,
    pub(crate) connection_limit: i32,
}

impl RoleDefinition {
    pub(crate) fn bootstrap() -> Self {
        Self {
            oid: 10,
            name: "uqa".into(),
            attributes: BTreeSet::from([
                RoleAttribute::Superuser,
                RoleAttribute::Inherit,
                RoleAttribute::CreateRole,
                RoleAttribute::CreateDb,
                RoleAttribute::Login,
                RoleAttribute::BypassRls,
            ]),
            connection_limit: -1,
        }
    }

    fn from_create(statement: &CreateRoleStmt) -> Self {
        Self {
            oid: role_oid(&statement.name),
            name: statement.name.clone(),
            attributes: statement.attributes.clone(),
            connection_limit: statement.connection_limit,
        }
    }

    pub(crate) fn has(&self, attribute: RoleAttribute) -> bool {
        self.attributes.contains(&attribute)
    }
}

pub(crate) fn role_oid(name: &str) -> i64 {
    if name == "uqa" {
        return 10;
    }
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    20_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

impl Engine {
    pub(crate) fn current_user_name(&self) -> String {
        self.session.state.read().current_user.clone()
    }

    pub(crate) fn session_user_name(&self) -> String {
        self.session.state.read().session_user.clone()
    }

    pub(crate) fn roles_for_catalog(&self) -> Vec<RoleDefinition> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return snapshot.roles.values().cloned().collect();
        }
        self.durable.roles.read().values().cloned().collect()
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
        match name {
            "CURRENT_USER" => self.current_user_name(),
            "SESSION_USER" => self.session_user_name(),
            other => other.to_string(),
        }
    }

    pub(crate) fn set_role(&self, requested: &str) -> Result<(), SQLError> {
        let target = if requested.is_empty()
            || requested.eq_ignore_ascii_case("none")
            || requested.eq_ignore_ascii_case("default")
        {
            self.session_user_name()
        } else {
            requested.to_string()
        };
        if !self.durable.roles.read().contains_key(&target) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{target}\" does not exist"),
            });
        }
        // The embedded connection starts as the bootstrap superuser. PostgreSQL lets a superuser session SET ROLE to any role even while a prior SET ROLE has reduced current_user.
        let session_user = self.session_user_name();
        let session_is_superuser = self
            .durable
            .roles
            .read()
            .get(&session_user)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
        if !session_is_superuser && target != session_user {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("permission denied to set role \"{target}\""),
            });
        }
        let mut state = self.session.state.write();
        state.current_user = target;
        state.sql_statement_cache.clear();
        Ok(())
    }

    pub(crate) fn create_role(&self, statement: &CreateRoleStmt) -> Result<(), SQLError> {
        self.require_role_administration("create role")?;
        if statement.attributes.contains(&RoleAttribute::Superuser)
            && !self.current_user_is_superuser()
        {
            return Err(insufficient_privilege(
                "must be superuser to create superusers",
            ));
        }
        self.prepare_explicit_transaction_writer()?;
        let mut roles = self.durable.roles.write();
        if roles.contains_key(&statement.name) {
            return Err(SQLError::Routine {
                sqlstate: "42710".into(),
                message: format!("role \"{}\" already exists", statement.name),
            });
        }
        let mut next = roles.clone();
        next.insert(
            statement.name.clone(),
            RoleDefinition::from_create(statement),
        );
        self.persist_roles_snapshot(&next)?;
        *roles = next;
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn alter_role(&self, statement: &AlterRoleStmt) -> Result<(), SQLError> {
        self.require_role_administration("alter role")?;
        let name = self.resolve_role_reference(&statement.name);
        let current = self.current_user_name();
        self.prepare_explicit_transaction_writer()?;
        let mut roles = self.durable.roles.write();
        let existing = roles.get(&name).cloned().ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{name}\" does not exist"),
        })?;
        let current_is_superuser = roles
            .get(&current)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
        if (statement.attributes.contains_key(&RoleAttribute::Superuser)
            || existing.has(RoleAttribute::Superuser))
            && !current_is_superuser
        {
            return Err(insufficient_privilege(
                "must be superuser to alter superuser roles or change superuser attribute",
            ));
        }
        let mut updated = existing;
        for (&attribute, &enabled) in &statement.attributes {
            if enabled {
                updated.attributes.insert(attribute);
            } else {
                updated.attributes.remove(&attribute);
            }
        }
        if let Some(value) = statement.connection_limit {
            updated.connection_limit = value;
        }
        let mut next = roles.clone();
        next.insert(name, updated);
        self.persist_roles_snapshot(&next)?;
        *roles = next;
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn drop_roles(&self, statement: &DropRoleStmt) -> Result<(), SQLError> {
        self.require_role_administration("drop role")?;
        let current = self.current_user_name();
        let session = self.session_user_name();
        self.prepare_explicit_transaction_writer()?;
        let mut roles = self.durable.roles.write();
        let snapshot = roles.clone();
        let mut names = Vec::new();
        for requested in &statement.names {
            let name = self.resolve_role_reference(requested);
            if !snapshot.contains_key(&name) {
                if statement.if_exists {
                    self.push_sql_notice(
                        "NOTICE",
                        &format!("role \"{name}\" does not exist, skipping"),
                    );
                    continue;
                }
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{name}\" does not exist"),
                });
            }
            if name == current || name == session {
                return Err(SQLError::Routine {
                    sqlstate: "55006".into(),
                    message: "current user cannot be dropped".into(),
                });
            }
            names.push(name);
        }
        let routines = self.durable.sql_user_functions.read();
        for name in &names {
            if let Some(dependent) = routines.values().flatten().find_map(|function| {
                let owns = function.def.owner == *name;
                let has_acl = function
                    .def
                    .execute_acl
                    .as_ref()
                    .is_some_and(|acl| acl.iter().any(|entry| entry.role == *name));
                (owns || has_acl).then(|| format!("routine {}", function.def.name))
            }) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!("role \"{name}\" cannot be dropped because some objects depend on it: {dependent}"),
                });
            }
        }
        let mut next = snapshot;
        for name in names {
            next.remove(&name);
        }
        self.persist_roles_snapshot(&next)?;
        *roles = next;
        drop(routines);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn require_role_administration(&self, action: &str) -> Result<(), SQLError> {
        let current = self.current_user_name();
        let allowed = self.durable.roles.read().get(&current).is_some_and(|role| {
            role.has(RoleAttribute::Superuser) || role.has(RoleAttribute::CreateRole)
        });
        if allowed {
            Ok(())
        } else {
            Err(insufficient_privilege(&format!(
                "permission denied to {action}"
            )))
        }
    }

    fn persist_roles_snapshot(
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

    pub(crate) fn restore_roles_from_metadata(
        &self,
        catalog: &dyn crate::CatalogFacade,
    ) -> StorageBackendResult<()> {
        let mut roles = match catalog.get_metadata(ROLES_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<BTreeMap<String, RoleDefinition>>(&json)?,
            None => BTreeMap::new(),
        };
        roles
            .entry("uqa".into())
            .or_insert_with(RoleDefinition::bootstrap);
        for (name, role) in &roles {
            if role.name != *name {
                return Err(StorageBackendError::Other(format!(
                    "persisted role key `{name}` does not match role name `{}`",
                    role.name
                )));
            }
        }
        *self.durable.roles.write() = roles;
        Ok(())
    }

    pub(crate) fn with_routine_context<T>(
        &self,
        definition: &CreateFunction,
        execute: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let _guard = self.routine_session_state_guard();
        let _volatility = RoutineVolatilityGuard::enter(definition.volatility);
        if definition.security.security_definer {
            self.session
                .state
                .write()
                .current_user
                .clone_from(&definition.owner);
        }
        for (name, value) in &definition.config {
            self.set_variable(name, value)?;
        }
        execute()
    }

    pub(crate) fn routine_session_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, false)
    }

    pub(crate) fn routine_config_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, true)
    }
}

fn insufficient_privilege(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42501".into(),
        message: message.into(),
    }
}
