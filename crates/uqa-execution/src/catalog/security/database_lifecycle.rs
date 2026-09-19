//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database ACL publication and restoration with retained authorization guards.

use super::roles::{
    dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
    locking::RoleLockContext,
};
use crate::row_locks::shared_objects::SharedObjectLockSession;
use std::{collections::BTreeSet, ops::DerefMut};
use uqa_sql::catalog::security::acl_command::{AclCommandRoles, ResolvedAclRoles};
use uqa_sql::{
    ast::GrantDatabaseStmt,
    catalog::{
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
        security::{
            database::{
                apply_database_acl, database_acl_warning, requested_acl_privileges,
                resolve_database_grant_targets, validate_database_acl_roles, BoundDatabaseSecurity,
            },
            database_inquiry::DatabaseSecurityRead,
            dependencies::added_acl_roles,
        },
        DATABASE_NAME,
    },
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendResult};

pub const DATABASE_SECURITY_METADATA_KEY: &str = "sql_database_security_json";
mod persistence;
pub type DatabaseSecurityWrite<'a> = Box<dyn DerefMut<Target = BoundDatabaseSecurity> + 'a>;

pub trait DatabaseSecurityRegistry {
    fn security_read(&self) -> DatabaseSecurityRead<'_>;
    fn security_write(&self) -> DatabaseSecurityWrite<'_>;
}
pub trait DatabasePrivilegePublication {
    fn prepare_writer(&self) -> Result<(), SQLError>;
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
    fn persist_security(&self, json: &str) -> Result<(), SQLError>;
    fn catalog_changed(&self);
    fn notice(&self, level: &str, message: &str);
}
pub struct DatabasePrivilegeContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub locks: &'a dyn SharedObjectLockSession,
    pub roles: &'a dyn RoleCatalogGuards,
    pub registry: &'a dyn DatabaseSecurityRegistry,
    pub publication: &'a dyn DatabasePrivilegePublication,
}

pub fn grant_database_privileges(
    context: &DatabasePrivilegeContext<'_>,
    statement: &GrantDatabaseStmt,
) -> Result<(), SQLError> {
    context
        .publication
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(format!("load database privileges: {error}")))?;
    let mut command_roles = AclCommandRoles::default();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value:
            DatabasePrivilegeCandidate {
                current,
                next,
                notice,
            },
        ..
    } = prepare_role_dependencies(
        &RoleLockContext {
            roles: context.roles,
            session: context.locks,
        },
        || context.publication.prepare_writer(),
        || prepare_privileges(context, statement, &mut command_roles),
    )?;
    if next != current {
        let json = persistence::encode(&next, &roles).map_err(|error| {
            SQLError::Internal(format!("serialize database privileges: {error}"))
        })?;
        context.publication.persist_security(&json)?;
        **context.registry.security_write() = next;
        context.publication.catalog_changed();
    }
    drop(memberships);
    drop(roles);
    if let Some((level, message)) = notice {
        context.publication.notice(level, &message);
    }
    Ok(())
}

struct DatabasePrivilegeCandidate {
    current: BoundDatabaseSecurity,
    next: BoundDatabaseSecurity,
    notice: Option<(&'static str, String)>,
}

fn prepare_privileges<'a>(
    context: &'a DatabasePrivilegeContext<'_>,
    statement: &GrantDatabaseStmt,
    command_roles: &mut AclCommandRoles,
) -> Result<RoleDependencyCandidate<'a, DatabasePrivilegeCandidate>, SQLError> {
    resolve_database_grant_targets(&statement.databases)?;
    let roles = context.roles.role_definitions();
    let ResolvedAclRoles {
        grantees,
        current_user,
        ..
    } = command_roles.resolve_validated(
        context.names,
        &roles,
        &statement.grantees,
        statement.grantor.as_ref(),
        |resolved| {
            validate_database_acl_roles(
                statement,
                &resolved.grantees,
                resolved.requested_grantor.as_deref(),
                &resolved.current_user,
                &roles,
            )
        },
    )?;
    let privileges = requested_acl_privileges(&statement.privileges)?;
    let memberships = context.roles.role_memberships();
    let current = context.registry.security_read().clone();
    let resolved = current.resolve(&roles).map_err(SQLError::Internal)?;
    let (next, grantable) = apply_database_acl(
        statement,
        &grantees,
        &privileges,
        &current_user,
        &roles,
        &memberships,
        &resolved,
    )?;
    let notice = (grantable != privileges.len())
        .then(|| database_acl_warning(statement.is_grant, grantable != 0, DATABASE_NAME));
    let mut dependencies = BTreeSet::new();
    added_acl_roles(
        resolved.acl.as_deref().unwrap_or_default(),
        &resolved.role_owner,
        next.acl.as_deref().unwrap_or_default(),
        &next.role_owner,
        &mut dependencies,
    );
    let next = BoundDatabaseSecurity::bind(&next, &roles).map_err(SQLError::Internal)?;
    Ok(RoleDependencyCandidate {
        value: DatabasePrivilegeCandidate {
            current,
            next,
            notice,
        },
        memberships,
        roles,
        dependencies,
    })
}

pub fn restore_database_security_from_metadata(
    context: &DatabasePrivilegeContext<'_>,
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<()> {
    let roles = context.roles.role_definitions();
    let security = persistence::restore(catalog, &roles, allow_migration)?;
    drop(roles);
    **context.registry.security_write() = security;
    Ok(())
}

#[cfg(test)]
mod tests;
