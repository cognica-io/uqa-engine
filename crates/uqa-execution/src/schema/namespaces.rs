//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema registration and owner publication through live catalog and authorization guards.
pub mod privileges;
pub mod removal;
pub mod restoration;

use crate::catalog::security::roles::RoleCatalogGuards;
use crate::catalog::security::roles::{
    dependencies::{prepare_role_owner, RoleDependencyCandidate},
    locking::RoleLockContext,
};
use crate::row_locks::shared_objects::SharedObjectLockSession;
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_sql::{
    catalog::{
        roles::{self, RoleReferenceNames},
        security::{schema::rewrite_schema_acl_owner, BoundSchemaSecurity},
    },
    SQLError, SQLResult,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub trait SchemaStatementWriter {
    fn prepare_writer(&self) -> Result<(), SQLError>;
}
pub trait NamespaceCatalogRefresh {
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
}
pub trait NamespaceCatalogChanges {
    fn catalog_registry_changed(&self);
}

pub type SchemaRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<String, BoundSchemaSecurity>> + 'a>;
pub trait SchemaRegistrationState {
    fn schemas_write(&self) -> SchemaRegistryWrite<'_>;
    fn contains_graph(&self, name: &str) -> bool;
}
pub trait SchemaRegistrationPersistence {
    fn persist_schema(
        &self,
        name: &str,
        security: &BoundSchemaSecurity,
    ) -> StorageBackendResult<()>;
}
pub struct SchemaRegistrationContext<'a> {
    pub state: &'a dyn SchemaRegistrationState,
    pub persistence: &'a dyn SchemaRegistrationPersistence,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn register_schema(
    context: &SchemaRegistrationContext<'_>,
    name: &str,
    if_not_exists: bool,
    role_owner: uqa_core::catalog_role::RoleIdentity,
) -> StorageBackendResult<bool> {
    uqa_sql::schema::namespaces::validate_schema_name(name).map_err(StorageBackendError::Other)?;
    let mut schemas = context.state.schemas_write();
    if schemas.contains_key(name) || context.state.contains_graph(name) {
        if if_not_exists {
            return Ok(false);
        }
        return Err(StorageBackendError::Other(format!(
            "schema `{name}` already exists"
        )));
    }
    let security = BoundSchemaSecurity {
        role_owner,
        acl: None,
    };
    context.persistence.persist_schema(name, &security)?;
    schemas.insert(name.to_string(), security);
    drop(schemas);
    context.changes.catalog_registry_changed();
    Ok(true)
}

pub trait SchemaRegistration {
    fn register_schema(
        &self,
        name: &str,
        if_not_exists: bool,
        role_owner: uqa_core::catalog_role::RoleIdentity,
    ) -> StorageBackendResult<bool>;
}
pub struct SchemaCreationContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub session: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub locks: &'a dyn SharedObjectLockSession,
    pub database: &'a dyn uqa_sql::catalog::security::database_inquiry::DatabasePrivilegeCatalog,
    pub registration: &'a dyn SchemaRegistration,
    pub catalog: &'a dyn SchemaSecurityCatalog,
    pub notices: &'a dyn crate::catalog::notices::CatalogNotices,
}

/// The host API keeps its declaration rules while sharing transactional owner dependency publication with SQL.
pub fn register_api_schema(
    context: &SchemaCreationContext<'_>,
    name: &str,
    if_not_exists: bool,
) -> Result<bool, SQLError> {
    context.locks.refresh_shared_catalog()?;
    uqa_sql::schema::namespaces::validate_schema_name(name).map_err(SQLError::Internal)?;
    let locks = RoleLockContext {
        roles: context.roles,
        session: context.locks,
    };
    let owner = locks.bind(&context.session.current_role())?;
    let RoleDependencyCandidate {
        roles,
        memberships,
        value,
        ..
    } = prepare_role_owner(
        locks,
        &owner,
        || context.writer.prepare_writer(),
        |_, _, _| {
            Ok(context
                .catalog
                .schema_security(name)
                .is_none()
                .then_some(()))
        },
    )?;
    let created = if value.is_some() {
        context
            .registration
            .register_schema(name, if_not_exists, owner.identity())
            .map_err(|error| uqa_sql::catalog::errors::storage_error("CREATE SCHEMA", &error))?
    } else if if_not_exists {
        false
    } else {
        return Err(SQLError::Internal(format!(
            "schema `{name}` already exists"
        )));
    };
    drop(memberships);
    drop(roles);
    Ok(created)
}

pub fn create_schema(
    context: &SchemaCreationContext<'_>,
    name: Option<&str>,
    if_not_exists: bool,
    authorization: Option<&uqa_sql::ast::SchemaAuthorization>,
) -> Result<SQLResult, SQLError> {
    let current_user = context.session.current_role();
    let target = uqa_sql::schema::namespaces::creation::schema_creation_target(
        context.session,
        context.roles,
        &current_user,
        name,
        authorization,
    )?;
    let locks = RoleLockContext {
        roles: context.roles,
        session: context.locks,
    };
    let owner = locks.bind(&target.role_owner)?;
    let RoleDependencyCandidate {
        roles,
        memberships,
        value,
        ..
    } = prepare_role_owner(
        locks,
        &owner,
        || context.writer.prepare_writer(),
        |roles, memberships, new_owner| {
            uqa_sql::catalog::security::ownership::OwnerChangeAuthority {
                roles,
                memberships,
                current_user: &current_user,
                new_owner,
            }
            .require_database_create(&context.database.security())?;
            roles::require_set_role(roles, memberships, &current_user, new_owner)?;
            uqa_sql::schema::namespaces::creation::validate_schema_creation_name(&target.name)?;
            Ok(context
                .catalog
                .schema_security(&target.name)
                .is_none()
                .then_some(()))
        },
    )?;
    let created = if value.is_none() {
        false
    } else {
        context
            .registration
            .register_schema(&target.name, true, owner.identity())
            .map_err(|error| {
                SQLError::Internal(format!("CREATE SCHEMA catalog write failed: {error}"))
            })?
    };
    drop(memberships);
    drop(roles);
    if !created {
        if !if_not_exists {
            return Err(SQLError::Routine {
                sqlstate: "42P06".into(),
                message: format!(r#"schema "{}" already exists"#, target.name),
            });
        }
        context.notices.notice(
            "NOTICE",
            &format!(r#"schema "{}" already exists, skipping"#, target.name),
        );
    }
    Ok(SQLResult::empty())
}

pub trait SchemaSecurityCatalog {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity>;
}
pub trait SchemaSecurityPersistence {
    fn persist_security(&self, name: &str, security: &BoundSchemaSecurity) -> Result<(), SQLError>;
}
pub trait SchemaSecurityPublication {
    fn publish_security(&self, name: &str, security: BoundSchemaSecurity);
}
pub struct SchemaOwnerContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub session: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub locks: &'a dyn SharedObjectLockSession,
    pub database: &'a dyn uqa_sql::catalog::security::database_inquiry::DatabasePrivilegeCatalog,
    pub catalog: &'a dyn SchemaSecurityCatalog,
    pub publication: &'a dyn SchemaSecurityPublication,
    pub persistence: &'a dyn SchemaSecurityPersistence,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn alter_schema_owner(
    context: &SchemaOwnerContext<'_>,
    name: &str,
    requested: &uqa_sql::ast::RoleSpecification,
) -> Result<(), SQLError> {
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let new_owner = roles::resolve_role_specification(context.session, requested);
    let locks = RoleLockContext {
        roles: context.roles,
        session: context.locks,
    };
    let owner = locks.bind(&new_owner)?;
    let current_user = context.session.current_role();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value,
        ..
    } = prepare_role_owner(
        locks,
        &owner,
        || context.writer.prepare_writer(),
        |roles, memberships, new_owner| {
            let security =
                context
                    .catalog
                    .schema_security(name)
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "3F000".into(),
                        message: format!("schema \"{name}\" does not exist"),
                    })?;
            if security.role_owner == owner.identity() {
                return Ok(None);
            }
            let mut security = security.resolve(roles).map_err(SQLError::Internal)?;
            let authority = uqa_sql::catalog::security::ownership::OwnerChangeAuthority {
                roles,
                memberships,
                current_user: &current_user,
                new_owner,
            };
            authority.require_owner_change(&security.role_owner, "schema", name)?;
            authority.require_database_create(&context.database.security())?;
            rewrite_schema_acl_owner(&mut security, new_owner);
            Ok(Some(
                BoundSchemaSecurity::bind(&security, roles).map_err(SQLError::Internal)?,
            ))
        },
    )?;
    let Some(security) = value else {
        return Ok(());
    };
    context.persistence.persist_security(name, &security)?;
    context.publication.publish_security(name, security);
    drop(memberships);
    drop(roles);
    context.changes.catalog_registry_changed();
    Ok(())
}

pub mod relations;
