//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace removal with ordered dependent-object reads, relation locks, and registry publication.

use super::{NamespaceCatalogChanges, NamespaceCatalogRefresh, SchemaRegistryWrite};
use crate::schema::removal::{
    RelationRemovalEvents, RelationRemovalLocks, RelationRemovalRoutines, RelationRemovalTables,
};
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
    fn schema_tables(&self, schemas: &BTreeSet<String>) -> Vec<String>;
    fn schema_foreign_tables(&self, schemas: &BTreeSet<String>) -> Vec<String>;
    fn schema_views(&self, schemas: &BTreeSet<String>) -> Vec<String>;
    fn schema_sequences(&self, schemas: &BTreeSet<String>) -> Vec<String>;
    fn graph_tables(&self, schema: &str) -> StorageBackendResult<Vec<String>>;
}
pub trait SchemaTypeRoutineRemoval {
    fn drop_schema_types_and_routines(&self, schemas: &BTreeSet<String>) -> Result<(), SQLError>;
}
pub trait SchemaRemovalViews {
    fn drop_views_depending_on_relations(&self, names: &[String]) -> StorageBackendResult<()>;
    fn remaining_view_drop_targets(&self, names: &[String]) -> Result<Vec<String>, SQLError>;
    fn cascade_view_closure(&self, names: Vec<String>) -> Result<Vec<String>, SQLError>;
    fn drop_views_inner(&self, names: &[String], cascade: bool) -> Result<(), SQLError>;
}
pub trait SchemaRemovalPublication {
    fn drop_graph(&self, name: &str) -> StorageBackendResult<()>;
    fn drop_empty_schema(&self, name: &str) -> StorageBackendResult<()>;
}
pub trait SchemaDropNotices {
    fn schema_drop_notice(&self, message: &str);
}
pub struct SchemaRemovalContext<'a> {
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub catalog: &'a dyn SchemaDropCatalog,
    pub names: &'a dyn SchemaRemovalNames,
    pub types: &'a dyn SchemaTypeRoutineRemoval,
    pub tables: &'a dyn RelationRemovalTables,
    pub routines: &'a dyn RelationRemovalRoutines,
    pub events: &'a dyn RelationRemovalEvents,
    pub foreign: crate::schema::foreign_removal::ForeignTableRemovalContext<'a>,
    pub views: &'a dyn SchemaRemovalViews,
    pub sequences: &'a dyn crate::schema::sequences::removal::SequenceRemovalInputs,
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
    if !validate_empty_schema_drop(context.catalog, name).map_err(StorageBackendError::Other)? {
        return Ok(false);
    }
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
    SQLError::Internal(format!("DROP SCHEMA: {error}"))
}

pub fn drop_schemas(
    context: &SchemaRemovalContext<'_>,
    statement: &DropStmt,
) -> Result<(), SQLError> {
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| storage_error(&error))?;
    let mut schemas = BTreeSet::new();
    let mut graphs = BTreeSet::new();
    for name in &statement.names {
        match bind_schema_drop_target(context.catalog, name, statement.if_exists)? {
            BoundSchemaDrop::Schema => {
                schemas.insert(name.clone());
            }
            BoundSchemaDrop::Graph => {
                graphs.insert(name.clone());
            }
            BoundSchemaDrop::Skipped(message) => context.notices.schema_drop_notice(&message),
        }
    }
    if !statement.cascade {
        validate_schema_drop_restrict(context.catalog, &schemas, &graphs)?;
    }
    if statement.cascade {
        context.types.drop_schema_types_and_routines(&schemas)?;
        drop_schema_relations(context, &schemas)?;
        let sequences = context.names.schema_sequences(&schemas);
        for sequence in sequences {
            context
                .sequences
                .sequence_removal_context()
                .drop_owned_sequence(&sequence, true)
                .map_err(|error| storage_error(&error))?;
        }
    }
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
    for schema in schemas {
        context
            .publication
            .drop_empty_schema(&schema)
            .map_err(|error| storage_error(&error))?;
    }
    Ok(())
}

fn drop_schema_relations(
    context: &SchemaRemovalContext<'_>,
    schemas: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let tables = context.names.schema_tables(schemas);
    let (tables, _) = context.tables.hierarchy_drop_targets(&tables, true);
    for table in &tables {
        context.locks.lock_exclusive(table)?;
        context
            .events
            .ensure_no_pending_trigger_events(table, "DROP TABLE")?;
    }
    if !tables.is_empty() {
        context
            .tables
            .try_drop_tables(&tables, true)
            .map_err(|error| storage_error(&error))?;
    }
    let foreign = context.names.schema_foreign_tables(schemas);
    let owned_sequences = context
        .foreign
        .foreign_table_owned_sequence_names(&foreign)
        .map_err(|error| storage_error(&error))?;
    for table in &foreign {
        context.locks.lock_exclusive(table)?;
    }
    context
        .events
        .drop_rules_depending_on_relations_inner(&foreign)
        .map_err(|error| storage_error(&error))?;
    context
        .views
        .drop_views_depending_on_relations(&foreign)
        .map_err(|error| storage_error(&error))?;
    for table in foreign {
        context
            .foreign
            .drop_foreign_table_inner(&table)
            .map_err(SQLError::Internal)?;
    }
    for sequence in owned_sequences {
        context
            .sequences
            .sequence_removal_context()
            .drop_owned_sequence(&sequence, true)
            .map_err(|error| storage_error(&error))?;
    }
    let views = context.names.schema_views(schemas);
    context
        .routines
        .drop_relation_routine_dependents(&views, true, "view")?;
    let views = context.views.remaining_view_drop_targets(&views)?;
    let closure = context.views.cascade_view_closure(views)?;
    for view in &closure {
        context.locks.lock_exclusive(view)?;
    }
    context
        .events
        .drop_rules_depending_on_relations_inner(&closure)
        .map_err(|error| storage_error(&error))?;
    context.views.drop_views_inner(&closure, false)
}
