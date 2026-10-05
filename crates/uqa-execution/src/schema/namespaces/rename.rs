//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ALTER SCHEMA ... RENAME TO` as `RenameSchema` performs it: the namespace keeps its identity, owner, privileges and dependents under the new name, and every object it holds follows, as `PostgreSQL`'s objects follow their namespace OID. The catalogs key relations, routines and types by their schema's name, so each member is relocated under the new name inside the statement's transaction after the schema itself has been checked.

use super::{
    locking, NamespaceCatalogChanges, NamespaceCatalogRefresh, SchemaRegistryWrite,
    SchemaSecurityCatalog, SchemaStatementWriter,
};
use crate::catalog::security::roles::RoleCatalogGuards;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::TypeObjectKind,
    catalog::{
        roles::{role_inherits, RoleReferenceNames},
        security::{
            database_inquiry::DatabasePrivilegeCatalog,
            ownership::{require_relation_ownership, OwnerChangeAuthority},
            BoundSchemaSecurity,
        },
    },
    schema::namespaces::creation::validate_schema_creation_name,
    SQLError,
};
use uqa_storage::StorageBackendResult;

/// The kinds of relation a schema holds, each moved by the lifecycle that renames it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaRelationKind {
    Table,
    View,
    MaterializedView,
    Sequence,
    ForeignTable,
}

/// One relation a schema holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRelation {
    pub identity: RelationIdentity,
    pub kind: SchemaRelationKind,
}

/// The durable schema registry and the names that are not schemas of their own.
pub trait SchemaRenameRegistry {
    fn schemas_write(&self) -> SchemaRegistryWrite<'_>;
    fn contains_graph(&self, name: &str) -> bool;
    fn temporary_schema_name(&self) -> String;
}

/// The objects a schema holds, as the catalogs list them.
pub trait SchemaMembers {
    fn relations(&self, schema: &str) -> StorageBackendResult<Vec<SchemaRelation>>;
    /// Registry keys of the routines the schema holds.
    fn routines(&self, schema: &str) -> Vec<String>;
    /// Qualified names of the enum, domain and composite types the schema holds, with the keyword that alters each.
    fn types(&self, schema: &str) -> Vec<(String, TypeObjectKind)>;
}

/// Move one member into another schema, rewriting the definitions that name it; the statement holds the schema's lock and has checked its ownership.
pub trait SchemaMemberRelocation {
    fn relocate_relation(&self, relation: &SchemaRelation, schema: &str) -> Result<(), SQLError>;
    fn relocate_routine(&self, registry_key: &str, schema: &str) -> Result<(), SQLError>;
    fn relocate_type(&self, name: &str, kind: TypeObjectKind, schema: &str)
        -> Result<(), SQLError>;
}

pub trait SchemaRenamePersistence {
    /// Store the schema's row under `name`.
    fn save_schema_row(&self, name: &str, security: &BoundSchemaSecurity) -> Result<(), SQLError>;
    /// Remove the row under `name` once it holds no relation.
    fn drop_schema_row(&self, name: &str) -> Result<(), SQLError>;
}

pub struct SchemaRenameContext<'a> {
    pub tuples: locking::SchemaLockContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub session: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub database: &'a dyn DatabasePrivilegeCatalog,
    pub catalog: &'a dyn SchemaSecurityCatalog,
    pub locks: &'a dyn crate::row_locks::shared_objects::SharedObjectLockSession,
    pub schemas: &'a dyn uqa_sql::catalog::security::schema_inquiry::SchemaPrivilegeCatalog,
    pub registry: &'a dyn SchemaRenameRegistry,
    pub members: &'a dyn SchemaMembers,
    pub relocation: &'a dyn SchemaMemberRelocation,
    pub persistence: &'a dyn SchemaRenamePersistence,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

/// Rename `name` to `new_name` with `RenameSchema`'s checks in its order: the schema must exist (`3F000`), the new name must be free (`42P06`), the current user must own the schema (`42501`) and hold `CREATE` on the database, and the new name must not be reserved (`42939`). The system schemas, the session's temporary schema and a graph's namespace cannot be renamed.
pub fn rename_schema(
    context: &SchemaRenameContext<'_>,
    name: &str,
    new_name: &str,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let temporary = context.registry.temporary_schema_name();
    if uqa_sql::catalog::is_virtual_system_schema(name) || name == temporary {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot rename system schema \"{name}\""),
        });
    }
    if context.registry.contains_graph(name) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot rename the namespace of graph \"{name}\""),
        });
    }
    context
        .tuples
        .bind_lifetime(name, crate::row_locks::RelationLockMode::AccessExclusive)?;
    context.tuples.catalog_write()?;
    let current = context
        .catalog
        .schema_security(name)
        .ok_or_else(|| locking::missing(name))?;
    if new_name == name
        || context.catalog.schema_security(new_name).is_some()
        || uqa_sql::catalog::is_virtual_system_schema(new_name)
        || context.registry.contains_graph(new_name)
        || new_name == temporary
    {
        return Err(SQLError::Routine {
            sqlstate: "42P06".into(),
            message: format!("schema \"{new_name}\" already exists"),
        });
    }
    let current_user = context.session.current_role();
    {
        let roles = context.roles.role_definitions();
        let memberships = context.roles.role_memberships();
        let security = current.resolve(&roles).map_err(SQLError::Internal)?;
        require_relation_ownership(
            name,
            "schema",
            role_inherits(&roles, &memberships, &current_user, &security.role_owner),
        )?;
        OwnerChangeAuthority {
            roles: &roles,
            memberships: &memberships,
            current_user: &current_user,
            new_owner: &security.role_owner,
        }
        .require_database_create(&context.database.security())?;
    }
    validate_schema_creation_name(new_name)?;
    // The schema's tuple is locked and checked under its current name before anything moves, as `RenameSchema` updates the pg_namespace row in place.
    context.tuples.replace(name, locking::tuple(&current)?)?;
    // Every refresh of the catalog while the members move must see one row per identity and every member's schema: the destination holds a tuple of its own until the old name is empty, then takes the schema's identity back.
    let transition = super::identity::reserve_namespace_tuple(
        &context.tuples,
        context.locks,
        context.catalog,
        context.schemas,
        new_name,
    )?;
    let mut staged = current.clone();
    staged.tuple = Some(transition);
    context
        .registry
        .schemas_write()
        .insert(new_name.to_string(), staged.clone());
    context.persistence.save_schema_row(new_name, &staged)?;
    let relations = context.members.relations(name).map_err(|error| {
        SQLError::Internal(format!("list the relations of schema `{name}`: {error}"))
    })?;
    for relation in &relations {
        context.relocation.relocate_relation(relation, new_name)?;
    }
    for routine in context.members.routines(name) {
        context.relocation.relocate_routine(&routine, new_name)?;
    }
    for (type_name, kind) in context.members.types(name) {
        context
            .relocation
            .relocate_type(&type_name, kind, new_name)?;
    }
    {
        let mut schemas = context.registry.schemas_write();
        schemas.remove(name);
        schemas.insert(new_name.to_string(), current.clone());
    }
    context.persistence.drop_schema_row(name)?;
    context.persistence.save_schema_row(new_name, &current)?;
    context.changes.namespace_catalog_changed();
    Ok(())
}
