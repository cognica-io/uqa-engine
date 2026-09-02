//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-shaped logical roles and routine execution contexts.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use uqa_sql::ast::{
    AlterRoleStmt, CreateFunction, CreateRoleStmt, DropRoleStmt, FunctionVolatility, GrantRoleStmt,
    RoleAttribute, RoleMembershipAction, RoleMembershipOptions,
};
use uqa_sql::SQLError;

use crate::{
    Engine, SQLStatementCache, StorageBackendError, StorageBackendResult, Value,
    ROLES_METADATA_KEY, ROLE_MEMBERSHIPS_METADATA_KEY,
};

pub(crate) struct RoutineSessionStateGuard<'a> {
    engine: &'a Engine,
    search_path: Vec<String>,
    session_vars: BTreeMap<String, String>,
    sql_statement_cache: Option<SQLStatementCache>,
    current_user: Option<String>,
}

thread_local! {
    static ROUTINE_VOLATILITY_STACK: RefCell<Vec<FunctionVolatility>> = const { RefCell::new(Vec::new()) };
    static SECURITY_DEFINER_DEPTH: Cell<usize> = const { Cell::new(0) };
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

struct SecurityDefinerGuard {
    active: bool,
}

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

impl SecurityDefinerGuard {
    fn enter(active: bool) -> Self {
        if active {
            SECURITY_DEFINER_DEPTH.with(|depth| depth.set(depth.get() + 1));
        }
        Self { active }
    }
}

impl Drop for SecurityDefinerGuard {
    fn drop(&mut self) {
        if self.active {
            SECURITY_DEFINER_DEPTH.with(|depth| {
                let current = depth.get();
                debug_assert!(current > 0, "security-definer depth underflow");
                depth.set(current.saturating_sub(1));
            });
        }
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
            current_user: Some(state.current_user.clone()),
        }
    }

    fn preserve_current_user(&mut self) {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoleDefinition {
    pub(crate) oid: i64,
    pub(crate) name: String,
    pub(crate) attributes: BTreeSet<RoleAttribute>,
    pub(crate) connection_limit: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RoleMembershipKey {
    pub(crate) role: String,
    pub(crate) member: String,
    pub(crate) grantor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoleMembership {
    pub(crate) oid: i64,
    pub(crate) role: String,
    pub(crate) member: String,
    pub(crate) grantor: String,
    pub(crate) admin_option: bool,
    pub(crate) inherit_option: bool,
    pub(crate) set_option: bool,
}

impl RoleMembership {
    fn key(&self) -> RoleMembershipKey {
        RoleMembershipKey {
            role: self.role.clone(),
            member: self.member.clone(),
            grantor: self.grantor.clone(),
        }
    }
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
        match name {
            "CURRENT_USER" => self.current_user_name(),
            "SESSION_USER" => self.session_user_name(),
            other => other.to_string(),
        }
    }

    pub(crate) fn set_role(&self, requested: &str) -> Result<(), SQLError> {
        if SECURITY_DEFINER_DEPTH.with(Cell::get) > 0 {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "cannot set parameter \"role\" within security-definer function".into(),
            });
        }
        let target = if requested.is_empty()
            || requested.eq_ignore_ascii_case("none")
            || requested.eq_ignore_ascii_case("default")
        {
            self.session_user_name()
        } else {
            requested.to_string()
        };
        let roles = self.durable.roles.read();
        if !roles.contains_key(&target) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{target}\" does not exist"),
            });
        }
        // The embedded connection starts as the bootstrap superuser. PostgreSQL lets a superuser session SET ROLE to any role even while a prior SET ROLE has reduced current_user.
        let session_user = self.session_user_name();
        let memberships = self.durable.role_memberships.read();
        if !role_can_set(&roles, &memberships, &session_user, &target) {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("permission denied to set role \"{target}\""),
            });
        }
        drop(memberships);
        drop(roles);
        let mut state = self.session.state.write();
        state.current_user = target;
        state.sql_statement_cache.clear();
        Ok(())
    }

    pub(crate) fn create_role(&self, statement: &CreateRoleStmt) -> Result<(), SQLError> {
        self.require_role_creation()?;
        let current = self.current_user_name();
        {
            let roles = self.durable.roles.read();
            require_role_attribute_authority(
                &roles,
                &current,
                statement.attributes.iter().copied(),
                "create role",
            )?;
        }
        self.prepare_explicit_transaction_writer()?;
        let mut roles = self.durable.roles.write();
        if roles.contains_key(&statement.name) {
            return Err(SQLError::Routine {
                sqlstate: "42710".into(),
                message: format!("role \"{}\" already exists", statement.name),
            });
        }
        let current_is_superuser = role_is_superuser(&roles, &current);
        let mut next_roles = roles.clone();
        next_roles.insert(
            statement.name.clone(),
            RoleDefinition::from_create(statement),
        );
        let mut memberships = self.durable.role_memberships.write();
        let mut next_memberships = memberships.clone();
        self.apply_create_role_memberships(
            statement,
            &current,
            current_is_superuser,
            &next_roles,
            &mut next_memberships,
        )?;
        self.persist_roles_snapshot(&next_roles)?;
        self.persist_role_memberships_snapshot(&next_memberships)?;
        *roles = next_roles;
        *memberships = next_memberships;
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn apply_create_role_memberships(
        &self,
        statement: &CreateRoleStmt,
        current: &str,
        current_is_superuser: bool,
        roles: &BTreeMap<String, RoleDefinition>,
        memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        if !current_is_superuser {
            let bootstrap = roles
                .values()
                .find(|role| role.has(RoleAttribute::Superuser))
                .map(|role| role.name.clone())
                .ok_or_else(|| {
                    SQLError::Internal("role catalog has no bootstrap superuser".into())
                })?;
            insert_membership(
                memberships,
                &statement.name,
                current,
                &bootstrap,
                RoleMembershipOptions {
                    admin: Some(true),
                    inherit: Some(false),
                    set: Some(false),
                },
                roles,
            );
        }
        let in_roles = statement
            .in_roles
            .iter()
            .map(|role| self.resolve_role_reference(role))
            .collect::<Vec<_>>();
        if !in_roles.is_empty() {
            apply_grant_role_statement(
                roles,
                memberships,
                current,
                &GrantRoleStmt {
                    granted_roles: in_roles,
                    grantee_roles: vec![statement.name.clone()],
                    is_grant: true,
                    options: RoleMembershipOptions::default(),
                    grantor: None,
                    cascade: false,
                },
            )?;
        }
        let role_members = statement
            .role_members
            .iter()
            .map(|role| self.resolve_role_reference(role))
            .collect::<Vec<_>>();
        if !role_members.is_empty() {
            apply_grant_role_statement(
                roles,
                memberships,
                current,
                &GrantRoleStmt {
                    granted_roles: vec![statement.name.clone()],
                    grantee_roles: role_members,
                    is_grant: true,
                    options: RoleMembershipOptions::default(),
                    grantor: None,
                    cascade: false,
                },
            )?;
        }
        let admin_members = statement
            .admin_members
            .iter()
            .map(|role| self.resolve_role_reference(role))
            .collect::<Vec<_>>();
        if !admin_members.is_empty() {
            apply_grant_role_statement(
                roles,
                memberships,
                current,
                &GrantRoleStmt {
                    granted_roles: vec![statement.name.clone()],
                    grantee_roles: admin_members,
                    is_grant: true,
                    options: RoleMembershipOptions {
                        admin: Some(true),
                        ..RoleMembershipOptions::default()
                    },
                    grantor: None,
                    cascade: false,
                },
            )?;
        }
        Ok(())
    }

    pub(crate) fn alter_role(&self, statement: &AlterRoleStmt) -> Result<(), SQLError> {
        if let Some(action) = statement.membership_action {
            return self.grant_roles(&GrantRoleStmt {
                granted_roles: vec![statement.name.clone()],
                grantee_roles: statement.members.clone(),
                is_grant: action == RoleMembershipAction::Add,
                options: RoleMembershipOptions::default(),
                grantor: None,
                cascade: false,
            });
        }
        let name = self.resolve_role_reference(&statement.name);
        let current = self.current_user_name();
        self.prepare_explicit_transaction_writer()?;
        let mut roles = self.durable.roles.write();
        let existing = roles.get(&name).cloned().ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{name}\" does not exist"),
        })?;
        self.require_role_administration_for(&roles, &current, &name, "alter role")?;
        require_role_attribute_authority(
            &roles,
            &current,
            statement.attributes.keys().copied(),
            "alter role",
        )?;
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
            self.require_role_administration_for(&snapshot, &current, &name, "drop role")?;
            names.push(name);
        }
        let names_set = names.iter().cloned().collect::<BTreeSet<_>>();
        let mut memberships = self.durable.role_memberships.write();
        for membership in memberships.values() {
            if names_set.contains(&membership.grantor)
                && !names_set.contains(&membership.role)
                && !names_set.contains(&membership.member)
            {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "role \"{}\" cannot be dropped because some objects depend on it: privileges for membership of role {} in role {}",
                        membership.grantor, membership.member, membership.role
                    ),
                });
            }
        }
        self.ensure_roles_have_no_object_dependencies(&names)?;
        let mut next_roles = snapshot;
        for name in &names {
            next_roles.remove(name);
        }
        let mut next_memberships = memberships.clone();
        next_memberships.retain(|_, membership| {
            !names_set.contains(&membership.role)
                && !names_set.contains(&membership.member)
                && !names_set.contains(&membership.grantor)
        });
        self.persist_roles_snapshot(&next_roles)?;
        self.persist_role_memberships_snapshot(&next_memberships)?;
        *roles = next_roles;
        *memberships = next_memberships;
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn ensure_roles_have_no_object_dependencies(&self, names: &[String]) -> Result<(), SQLError> {
        let database_security = self.durable.database_security.read();
        for name in names {
            if database_depends_on_role(&database_security, name) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "role \"{name}\" cannot be dropped because some objects depend on it: database uqa"
                    ),
                });
            }
        }
        drop(database_security);

        let schema_security = self.durable.schemas.read();
        for name in names {
            if let Some(schema) = dependent_schema_for_role(&schema_security, name) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "role \"{name}\" cannot be dropped because some objects depend on it: schema {schema}"
                    ),
                });
            }
        }
        drop(schema_security);

        let tables = self.storage.tables.read();
        for name in names {
            if let Some(relation) = tables.iter().find_map(|(relation, table)| {
                table_security_depends_on_role(&table.security(), name).then_some(relation)
            }) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "role \"{name}\" cannot be dropped because some objects depend on it: table {}",
                        relation.qualified_name()
                    ),
                });
            }
        }
        drop(tables);

        let sequence_security = self.durable.sequence_security.read();
        for name in names {
            if let Some(relation) = dependent_sequence_for_role(&sequence_security, name) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "role \"{name}\" cannot be dropped because some objects depend on it: sequence {}",
                        relation.qualified_name()
                    ),
                });
            }
        }
        drop(sequence_security);

        let routines = self.durable.sql_user_functions.read();
        for name in names {
            if let Some(dependent) = routines.values().flatten().find_map(|function| {
                let owns = function.def.owner == *name;
                let has_acl = function.def.execute_acl.as_ref().is_some_and(|acl| {
                    acl.iter().any(|entry| {
                        entry.role == *name
                            || entry.grantor.as_deref().unwrap_or(&function.def.owner) == name
                    })
                });
                (owns || has_acl).then(|| format!("routine {}", function.def.name))
            }) {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!("role \"{name}\" cannot be dropped because some objects depend on it: {dependent}"),
                });
            }
        }
        Ok(())
    }

    fn require_role_creation(&self) -> Result<(), SQLError> {
        let current = self.current_user_name();
        let allowed = self.durable.roles.read().get(&current).is_some_and(|role| {
            role.has(RoleAttribute::Superuser) || role.has(RoleAttribute::CreateRole)
        });
        if allowed {
            Ok(())
        } else {
            Err(insufficient_privilege("permission denied to create role"))
        }
    }

    fn require_role_administration_for(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
        current: &str,
        target: &str,
        action: &str,
    ) -> Result<(), SQLError> {
        if role_is_superuser(roles, current) {
            return Ok(());
        }
        let can_create_roles = roles
            .get(current)
            .is_some_and(|role| role.has(RoleAttribute::CreateRole));
        let memberships = self.durable.role_memberships.read();
        if can_create_roles && role_has_admin(&memberships, current, target) {
            Ok(())
        } else {
            Err(insufficient_privilege(&format!(
                "permission denied to {action}"
            )))
        }
    }

    pub(crate) fn grant_roles(&self, statement: &GrantRoleStmt) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let resolved = GrantRoleStmt {
            granted_roles: statement
                .granted_roles
                .iter()
                .map(|role| self.resolve_role_reference(role))
                .collect(),
            grantee_roles: statement
                .grantee_roles
                .iter()
                .map(|role| self.resolve_role_reference(role))
                .collect(),
            is_grant: statement.is_grant,
            options: statement.options,
            grantor: statement
                .grantor
                .as_ref()
                .map(|role| self.resolve_role_reference(role)),
            cascade: statement.cascade,
        };
        let roles = self.durable.roles.read();
        let current = self.current_user_name();
        let mut memberships = self.durable.role_memberships.write();
        let mut next = memberships.clone();
        apply_grant_role_statement(&roles, &mut next, &current, &resolved)?;
        self.persist_role_memberships_snapshot(&next)?;
        *memberships = next;
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn current_user_has_role_privileges(&self, target: &str) -> bool {
        let current = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        role_inherits(&roles, &memberships, &current, target)
    }

    pub(crate) fn pg_has_role_value(&self, arguments: &[Value]) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, target_value, privilege_value) = match arguments {
            [target, privilege] => (None, target, privilege),
            [subject, target, privilege] => (Some(subject), target, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "pg_has_role".into(),
                    expected: "2 or 3".into(),
                    actual: arguments.len(),
                });
            }
        };
        let current_user = subject_value.is_none().then(|| self.current_user_name());
        let roles = self.durable.roles.read();
        let subject = subject_value.map_or_else(
            || Ok(current_user),
            |value| resolve_pg_has_role_identifier(value, &roles),
        )?;
        let target = resolve_pg_has_role_identifier(target_value, &roles)?;
        let privileges = parse_pg_has_role_privileges(role_privilege_text(privilege_value)?)?;
        let memberships = self.durable.role_memberships.read();
        let allowed = privileges.into_iter().any(|privilege| {
            pg_has_role_privilege(
                &roles,
                &memberships,
                subject.as_deref(),
                target.as_deref(),
                privilege,
            )
        });
        Ok(Value::Bool(allowed))
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

    fn persist_role_memberships_snapshot(
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
        let memberships = match catalog.get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<Vec<RoleMembership>>(&json)?,
            None => Vec::new(),
        };
        let mut membership_map = BTreeMap::new();
        let mut membership_oids = BTreeSet::new();
        for membership in memberships {
            if !roles.contains_key(&membership.role)
                || !roles.contains_key(&membership.member)
                || !roles.contains_key(&membership.grantor)
            {
                return Err(StorageBackendError::Other(format!(
                    "persisted role membership `{}` -> `{}` has a missing role or grantor",
                    membership.member, membership.role
                )));
            }
            if !membership_oids.insert(membership.oid) {
                return Err(StorageBackendError::Other(format!(
                    "persisted role membership OID {} is duplicated",
                    membership.oid
                )));
            }
            let key = membership.key();
            if membership_map.insert(key, membership).is_some() {
                return Err(StorageBackendError::Other(
                    "persisted role membership identity is duplicated".into(),
                ));
            }
        }
        *self.durable.roles.write() = roles;
        *self.durable.role_memberships.write() = membership_map;
        Ok(())
    }

    pub(crate) fn with_routine_context<T>(
        &self,
        definition: &CreateFunction,
        execute: impl FnOnce() -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let mut guard = self.routine_session_state_guard();
        let _volatility = RoutineVolatilityGuard::enter(definition.volatility);
        let _security_definer = SecurityDefinerGuard::enter(definition.security.security_definer);
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
        let result = execute();
        if result.is_ok() && !definition.security.security_definer {
            guard.preserve_current_user();
        }
        result
    }

    pub(crate) fn routine_session_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, false)
    }

    pub(crate) fn routine_config_state_guard(&self) -> RoutineSessionStateGuard<'_> {
        RoutineSessionStateGuard::capture(self, true)
    }
}

fn table_security_depends_on_role(
    security: &crate::engine_state::TableSecurity,
    role: &str,
) -> bool {
    let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
        acl.iter().any(|entry| {
            entry.role == role || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
        })
    });
    security.role_owner == role || acl_dependency
}

fn dependent_sequence_for_role<'a>(
    sequences: &'a BTreeMap<crate::RelationIdentity, crate::engine_state::SequenceSecurity>,
    role: &str,
) -> Option<&'a crate::RelationIdentity> {
    sequences.iter().find_map(|(relation, security)| {
        let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
            acl.iter().any(|entry| {
                entry.role == role
                    || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
            })
        });
        (security.role_owner == role || acl_dependency).then_some(relation)
    })
}

fn dependent_schema_for_role<'a>(
    schemas: &'a BTreeMap<String, crate::engine_state::SchemaSecurity>,
    role: &str,
) -> Option<&'a String> {
    schemas.iter().find_map(|(name, security)| {
        let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
            acl.iter().any(|entry| {
                entry.role == role
                    || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
            })
        });
        (security.role_owner == role || acl_dependency).then_some(name)
    })
}

fn database_depends_on_role(security: &crate::engine_state::DatabaseSecurity, role: &str) -> bool {
    let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
        acl.iter().any(|entry| {
            entry.role == role || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
        })
    });
    security.role_owner == role || acl_dependency
}

mod memberships;
use memberships::{
    apply_grant_role_statement, insert_membership, insufficient_privilege,
    parse_pg_has_role_privileges, pg_has_role_privilege, require_role_attribute_authority,
    resolve_pg_has_role_identifier, role_has_admin, role_is_superuser, role_privilege_text,
};
pub(crate) use memberships::{role_can_set, role_inherits};
