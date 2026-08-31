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

    pub(crate) fn role_memberships_for_catalog(&self) -> Vec<RoleMembership> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return snapshot.role_memberships.values().cloned().collect();
        }
        self.durable
            .role_memberships
            .read()
            .values()
            .cloned()
            .collect()
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
        drop(routines);
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
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

fn role_is_superuser(roles: &BTreeMap<String, RoleDefinition>, role: &str) -> bool {
    roles
        .get(role)
        .is_some_and(|definition| definition.has(RoleAttribute::Superuser))
}

fn require_role_attribute_authority(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &str,
    attributes: impl IntoIterator<Item = RoleAttribute>,
    action: &str,
) -> Result<(), SQLError> {
    let current_role = roles.get(current).ok_or_else(|| undefined_role(current))?;
    if current_role.has(RoleAttribute::Superuser) {
        return Ok(());
    }
    for attribute in attributes {
        let restricted = matches!(
            attribute,
            RoleAttribute::Superuser
                | RoleAttribute::CreateRole
                | RoleAttribute::CreateDb
                | RoleAttribute::Replication
                | RoleAttribute::BypassRls
        );
        if restricted && !current_role.has(attribute) {
            return Err(insufficient_privilege(&format!(
                "permission denied to {action}"
            )));
        }
    }
    Ok(())
}

fn role_has_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    memberships.values().any(|membership| {
        membership.member == member && membership.role == role && membership.admin_option
    })
}

fn role_reaches(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
    usable: impl Fn(&RoleMembership) -> bool,
) -> bool {
    if member == role {
        return true;
    }
    let mut queue = VecDeque::from([member.to_string()]);
    let mut visited = BTreeSet::from([member.to_string()]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships
            .values()
            .filter(|membership| membership.member == current && usable(membership))
        {
            if membership.role == role {
                return true;
            }
            if visited.insert(membership.role.clone()) {
                queue.push_back(membership.role.clone());
            }
        }
    }
    false
}

pub(crate) fn role_can_set(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    role_is_superuser(roles, member)
        || role_reaches(memberships, member, role, |membership| {
            membership.set_option
        })
}

pub(crate) fn role_inherits(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    role_is_superuser(roles, member)
        || role_reaches(memberships, member, role, |membership| {
            membership.inherit_option
        })
}

fn membership_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0LP01".into(),
        message: message.into(),
    }
}

fn undefined_role(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    }
}

fn apply_grant_role_statement(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &str,
    statement: &GrantRoleStmt,
) -> Result<(), SQLError> {
    for role in statement
        .granted_roles
        .iter()
        .chain(statement.grantee_roles.iter())
    {
        if !roles.contains_key(role) {
            return Err(undefined_role(role));
        }
    }
    let grantor = statement.grantor.as_deref().unwrap_or(current);
    if !roles.contains_key(grantor) {
        return Err(undefined_role(grantor));
    }
    if statement.grantor.is_some() && !role_can_set(roles, memberships, current, grantor) {
        return Err(insufficient_privilege(&format!(
            "permission denied to grant privileges as role \"{grantor}\""
        )));
    }
    for role in &statement.granted_roles {
        let superuser_revoke = !statement.is_grant && role_is_superuser(roles, current);
        if !superuser_revoke
            && !role_is_superuser(roles, grantor)
            && !role_has_admin(memberships, grantor, role)
        {
            return Err(insufficient_privilege(&format!(
                "permission denied to {} role \"{role}\"",
                if statement.is_grant {
                    "grant"
                } else {
                    "revoke"
                }
            )));
        }
    }
    for role in &statement.granted_roles {
        for member in &statement.grantee_roles {
            let key = RoleMembershipKey {
                role: role.clone(),
                member: member.clone(),
                grantor: grantor.to_string(),
            };
            if statement.is_grant {
                if role_reaches(memberships, role, member, |_| true) {
                    return Err(membership_error(format!(
                        "role \"{role}\" is a member of role \"{member}\""
                    )));
                }
                insert_membership(memberships, role, member, grantor, statement.options, roles);
            } else if statement.options == RoleMembershipOptions::default() {
                revoke_membership(memberships, &key, statement.cascade, true)?;
            } else if let Some(existing) = memberships.get(&key).cloned() {
                if statement.options.admin == Some(false) && existing.admin_option {
                    clear_membership_admin(memberships, &key, statement.cascade)?;
                }
                if let Some(membership) = memberships.get_mut(&key) {
                    if statement.options.inherit == Some(false) {
                        membership.inherit_option = false;
                    }
                    if statement.options.set == Some(false) {
                        membership.set_option = false;
                    }
                }
            }
        }
    }
    Ok(())
}

fn insert_membership(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &str,
    member: &str,
    grantor: &str,
    options: RoleMembershipOptions,
    roles: &BTreeMap<String, RoleDefinition>,
) {
    let key = RoleMembershipKey {
        role: role.to_string(),
        member: member.to_string(),
        grantor: grantor.to_string(),
    };
    if let Some(existing) = memberships.get_mut(&key) {
        if let Some(value) = options.admin {
            existing.admin_option = value;
        }
        if let Some(value) = options.inherit {
            existing.inherit_option = value;
        }
        if let Some(value) = options.set {
            existing.set_option = value;
        }
        return;
    }
    let oid = allocate_role_membership_oid(memberships, &key);
    memberships.insert(
        key,
        RoleMembership {
            oid,
            role: role.to_string(),
            member: member.to_string(),
            grantor: grantor.to_string(),
            admin_option: options.admin.unwrap_or(false),
            inherit_option: options.inherit.unwrap_or_else(|| {
                roles
                    .get(member)
                    .is_some_and(|role| role.has(RoleAttribute::Inherit))
            }),
            set_option: options.set.unwrap_or(true),
        },
    );
}

fn allocate_role_membership_oid(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for part in [&key.role, &key.member, &key.grantor] {
        for byte in part.as_bytes().iter().copied().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
    }
    let mut oid = 2_500_000_000_i64 + i64::try_from(hash % 1_500_000_000).unwrap_or(0);
    while memberships.values().any(|membership| membership.oid == oid) {
        oid = if oid == 3_999_999_999 {
            2_500_000_000
        } else {
            oid + 1
        };
    }
    oid
}

fn clear_membership_admin(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
    cascade: bool,
) -> Result<(), SQLError> {
    let Some(existing) = memberships.get_mut(key) else {
        return Ok(());
    };
    existing.admin_option = false;
    revoke_dependent_memberships(memberships, &key.role, &key.member, cascade)
}

fn revoke_membership(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
    cascade: bool,
    check_dependents: bool,
) -> Result<(), SQLError> {
    let Some(existing) = memberships.remove(key) else {
        return Ok(());
    };
    if check_dependents && existing.admin_option {
        revoke_dependent_memberships(memberships, &existing.role, &existing.member, cascade)?;
    }
    Ok(())
}

fn revoke_dependent_memberships(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &str,
    former_admin: &str,
    cascade: bool,
) -> Result<(), SQLError> {
    if role_has_admin(memberships, former_admin, role) {
        return Ok(());
    }
    let dependent = memberships
        .iter()
        .filter(|(_, membership)| membership.role == role && membership.grantor == former_admin)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    if dependent.is_empty() {
        return Ok(());
    }
    if !cascade {
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: "dependent privileges exist".into(),
        });
    }
    for key in dependent {
        revoke_membership(memberships, &key, true, true)?;
    }
    Ok(())
}

fn insufficient_privilege(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42501".into(),
        message: message.into(),
    }
}
