//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace removal with ordered dependent-object reads, relation locks, and registry publication.

use super::{NamespaceCatalogChanges, NamespaceCatalogRefresh, SchemaRegistryWrite};
use crate::schema::removal::RelationRemovalLocks;
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::DropStmt,
    schema::namespaces::removal::{
        bind_schema_drop_target, validate_empty_schema_drop, validate_schema_drop_restrict,
        BoundSchemaDrop, EmptySchemaCatalog, SchemaDropCatalog,
    },
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub trait SchemaRemovalNames {
    fn graph_tables(&self, schema: &str) -> StorageBackendResult<Vec<String>>;
}
pub trait SchemaRemovalPublication {
    fn drop_graph(&self, name: &str) -> StorageBackendResult<()>;
}
pub trait SchemaDropNotices {
    fn schema_drop_notice(&self, notice: uqa_sql::SQLNotice);
}
pub struct SchemaRemovalContext<'a> {
    pub deletion: &'a dyn crate::schema::deletion::CatalogRemovalInputs,
    pub tuples: super::locking::SchemaLockContext<'a>,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub catalog: &'a dyn SchemaDropCatalog,
    pub names: &'a dyn SchemaRemovalNames,
    pub locks: &'a dyn RelationRemovalLocks,
    pub publication: &'a dyn SchemaRemovalPublication,
    pub notices: &'a dyn SchemaDropNotices,
}
pub trait EmptySchemaRemovalState {
    fn schema_registry_write(&self) -> SchemaRegistryWrite<'_>;
}
pub trait EmptySchemaRemovalPersistence {
    fn drop_schema_row(&self, name: &str) -> StorageBackendResult<()>;
}
pub struct EmptySchemaRemovalContext<'a> {
    pub tuples: super::locking::SchemaLockContext<'a>,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub catalog: &'a dyn EmptySchemaCatalog,
    pub state: &'a dyn EmptySchemaRemovalState,
    pub persistence: &'a dyn EmptySchemaRemovalPersistence,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn drop_empty_schema(
    context: &EmptySchemaRemovalContext<'_>,
    name: &str,
) -> StorageBackendResult<bool> {
    context.refresh.refresh_catalog()?;
    let Some(_) = context
        .tuples
        .bind_lifetime(name, crate::row_locks::RelationLockMode::AccessExclusive)
        .map_err(|error| StorageBackendError::backend("DROP SCHEMA", error))?
    else {
        return Ok(false);
    };
    if !validate_empty_schema_drop(context.catalog, name).map_err(StorageBackendError::Other)? {
        return Ok(false);
    }
    lock_deletion(&context.tuples, name)
        .map_err(|error| StorageBackendError::backend("DROP SCHEMA", error))?;
    let mut schemas = context.state.schema_registry_write();
    context.persistence.drop_schema_row(name)?;
    let removed = schemas.remove(name).is_some();
    drop(schemas);
    if removed {
        context.changes.catalog_registry_changed();
    }
    Ok(removed)
}

fn storage_error(error: &StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error("DROP SCHEMA", error)
}

pub fn drop_schemas(
    context: &SchemaRemovalContext<'_>,
    statement: &DropStmt,
) -> Result<(), SQLError> {
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| storage_error(&error))?;
    let mut schemas = Vec::new();
    let mut graphs = BTreeSet::new();
    for name in &statement.names {
        let mut target = bind_schema_drop_target(context.catalog, name, statement.if_exists)?;
        if matches!(target, BoundSchemaDrop::Schema) {
            context
                .tuples
                .bind_lifetime(name, crate::row_locks::RelationLockMode::AccessExclusive)?;
            target = bind_schema_drop_target(context.catalog, name, statement.if_exists)?;
        }
        match target {
            BoundSchemaDrop::Schema => {
                if !schemas.contains(name) {
                    schemas.push(name.clone());
                }
            }
            BoundSchemaDrop::Graph => {
                graphs.insert(name.clone());
            }
            BoundSchemaDrop::Skipped(message) => context
                .notices
                .schema_drop_notice(uqa_sql::SQLNotice::notice(message)),
        }
    }
    if !statement.cascade && !graphs.is_empty() {
        validate_schema_drop_restrict(
            context.catalog,
            &schemas.iter().cloned().collect(),
            &graphs,
        )?;
    }
    crate::schema::deletion::perform_deletion(
        &context.deletion.catalog_removal_context(),
        |dependencies| {
            schemas
                .iter()
                .map(|schema| {
                    crate::schema::deletion::required_address(
                        dependencies.schema_address(schema),
                        || format!("schema {schema}"),
                    )
                })
                .collect()
        },
        statement.cascade,
    )?;
    for graph in graphs {
        for table in context
            .names
            .graph_tables(&graph)
            .map_err(|error| storage_error(&error))?
        {
            let relation = RelationIdentity {
                schema: graph.clone(),
                name: table,
            };
            context.locks.lock_exclusive(&relation.qualified_name())?;
        }
        context
            .publication
            .drop_graph(&graph)
            .map_err(|error| storage_error(&error))?;
    }
    Ok(())
}

fn lock_deletion(
    context: &super::locking::SchemaLockContext<'_>,
    name: &str,
) -> Result<(), SQLError> {
    context.catalog_write()?;
    let security = context
        .catalog
        .schema_security(name)
        .ok_or_else(|| super::locking::missing(name))?;
    context.replace(name, super::locking::tuple(&security)?)
}
