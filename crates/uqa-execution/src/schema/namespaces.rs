//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema registration and owner publication through live catalog and authorization guards.
pub mod privileges;

use crate::catalog::security::roles::RoleCatalogGuards;
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_sql::{
    catalog::{
        roles::{self, RoleReferenceNames},
        security::{schema::rewrite_schema_acl_owner, SchemaSecurity},
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
pub trait SchemaAuthority {
    fn current_user_has_role_privileges(&self, role: &str) -> bool;
    fn current_user_is_superuser(&self) -> bool;
    fn ensure_database_create(&self, role: &str) -> Result<(), SQLError>;
}

pub type SchemaRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<String, SchemaSecurity>> + 'a>;
pub trait SchemaRegistrationState {
    fn schemas_write(&self) -> SchemaRegistryWrite<'_>;
    fn contains_graph(&self, name: &str) -> bool;
}
pub trait SchemaRegistrationPersistence {
    fn persist_schema(&self, name: &str, security: &SchemaSecurity) -> StorageBackendResult<()>;
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
    role_owner: &str,
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
    let security = SchemaSecurity {
        role_owner: role_owner.to_string(),
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
        role_owner: &str,
    ) -> StorageBackendResult<bool>;
}
pub struct SchemaCreationContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub session: &'a dyn RoleReferenceNames,
    pub authority: &'a dyn SchemaAuthority,
    pub registration: &'a dyn SchemaRegistration,
}

pub fn create_schema(
    context: &SchemaCreationContext<'_>,
    name: &str,
    if_not_exists: bool,
) -> Result<SQLResult, SQLError> {
    context.writer.prepare_writer()?;
    let role_owner = context.session.current_user_name();
    context.authority.ensure_database_create(&role_owner)?;
    context
        .registration
        .register_schema(name, if_not_exists, &role_owner)
        .map_err(|error| {
            SQLError::Internal(format!("CREATE SCHEMA catalog write failed: {error}"))
        })?;
    Ok(SQLResult::empty())
}

pub trait SchemaSecurityCatalog {
    fn schema_security(&self, name: &str) -> Option<SchemaSecurity>;
}
pub trait SchemaSecurityPersistence {
    fn persist_security(&self, name: &str, security: &SchemaSecurity) -> Result<(), SQLError>;
}
pub trait SchemaSecurityPublication {
    fn publish_security(&self, name: &str, security: SchemaSecurity);
}
pub struct SchemaOwnerContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub session: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub authority: &'a dyn SchemaAuthority,
    pub catalog: &'a dyn SchemaSecurityCatalog,
    pub persistence: &'a dyn SchemaSecurityPersistence,
    pub publication: &'a dyn SchemaSecurityPublication,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn alter_schema_owner(
    context: &SchemaOwnerContext<'_>,
    name: &str,
    requested: &str,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let new_owner = roles::resolve_role_reference(context.session, requested);
    roles::require_role_exists(&context.roles.role_definitions(), &new_owner)?;
    let mut security = context
        .catalog
        .schema_security(name)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{name}\" does not exist"),
        })?;
    if security.role_owner == new_owner {
        return Ok(());
    }
    if !context
        .authority
        .current_user_has_role_privileges(&security.role_owner)
    {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of schema {name}"),
        });
    }
    if !context.authority.current_user_is_superuser() {
        roles::require_set_role(
            &context.roles.role_definitions(),
            &context.roles.role_memberships(),
            &context.session.current_user_name(),
            &new_owner,
        )?;
        context.authority.ensure_database_create(&new_owner)?;
    }
    rewrite_schema_acl_owner(&mut security, &new_owner);
    context.persistence.persist_security(name, &security)?;
    context.publication.publish_security(name, security);
    context.changes.catalog_registry_changed();
    Ok(())
}
