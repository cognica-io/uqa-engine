//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database ACL publication and restoration with retained authorization guards.

use std::ops::DerefMut;
use uqa_sql::{
    ast::GrantDatabaseStmt,
    catalog::{
        roles::{guards::RoleCatalogGuards, resolve_role_reference, RoleReferenceNames},
        security::{
            database::{
                apply_database_acl, database_acl_warning, requested_acl_privileges,
                resolve_database_grant_targets, validate_database_acl_roles,
                validate_stored_database_security, DatabaseSecurity,
            },
            database_inquiry::DatabaseSecurityRead,
        },
        DATABASE_NAME,
    },
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub const DATABASE_SECURITY_METADATA_KEY: &str = "sql_database_security_json";
pub type DatabaseSecurityWrite<'a> = Box<dyn DerefMut<Target = DatabaseSecurity> + 'a>;

pub trait DatabaseSecurityRegistry {
    fn security_read(&self) -> DatabaseSecurityRead<'_>;
    fn security_write(&self) -> DatabaseSecurityWrite<'_>;
}
pub trait DatabasePrivilegePublication {
    fn prepare_writer(&self) -> Result<(), SQLError>;
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
    fn persist_security(&self, security: &DatabaseSecurity) -> Result<(), SQLError>;
    fn catalog_changed(&self);
    fn notice(&self, level: &str, message: &str);
}
pub struct DatabasePrivilegeContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub registry: &'a dyn DatabaseSecurityRegistry,
    pub publication: &'a dyn DatabasePrivilegePublication,
}

pub fn grant_database_privileges(
    context: &DatabasePrivilegeContext<'_>,
    statement: &GrantDatabaseStmt,
) -> Result<(), SQLError> {
    context.publication.prepare_writer()?;
    context
        .publication
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(format!("load database privileges: {error}")))?;
    resolve_database_grant_targets(&statement.databases)?;
    let grantees = statement
        .grantees
        .iter()
        .map(|role| resolve_role_reference(context.names, role))
        .collect::<Vec<_>>();
    let requested_grantor = statement
        .grantor
        .as_ref()
        .map(|role| resolve_role_reference(context.names, role));
    let current_user = context.names.current_user_name();
    let roles = context.roles.role_definitions();
    validate_database_acl_roles(
        statement,
        &grantees,
        requested_grantor.as_deref(),
        &current_user,
        &roles,
    )?;
    let privileges = requested_acl_privileges(&statement.privileges)?;
    let memberships = context.roles.role_memberships();
    let current = context.registry.security_read().clone();
    let (next, grantable) = apply_database_acl(
        statement,
        &grantees,
        &privileges,
        &current_user,
        &roles,
        &memberships,
        &current,
    )?;
    let notice = (grantable != privileges.len())
        .then(|| database_acl_warning(statement.is_grant, grantable != 0, DATABASE_NAME));
    if next != current {
        context.publication.persist_security(&next)?;
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

pub fn restore_database_security_from_metadata(
    context: &DatabasePrivilegeContext<'_>,
    catalog: &dyn CatalogFacade,
) -> StorageBackendResult<()> {
    let security = match catalog.get_metadata(DATABASE_SECURITY_METADATA_KEY)? {
        Some(json) => serde_json::from_str::<DatabaseSecurity>(&json)?,
        None => DatabaseSecurity::bootstrap(),
    };
    let roles = context.roles.role_definitions();
    validate_stored_database_security(&security, &roles).map_err(StorageBackendError::Other)?;
    drop(roles);
    **context.registry.security_write() = security;
    Ok(())
}

#[cfg(test)]
mod tests;
