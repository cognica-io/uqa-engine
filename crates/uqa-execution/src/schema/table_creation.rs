//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute table creation with the original namespace, sequence, and schema write boundaries.
use super::{
    publication::{self, SchemaWriteTransaction},
    sequences::{
        implicit::{self, ImplicitSequenceContext},
        ownership::{self, ImplicitOwnershipContext},
    },
};
use uqa_sql::ast::{
    ColumnType, CreateTable, DeferredCreateTable, OnCommitAction, RelationPersistence,
    TableConstraintSet, TableHierarchy,
};
use uqa_sql::schema::table_creation::{
    declaration::{self, CreateTableAnalysisContext},
    validate_create_table_columns,
};
use uqa_sql::{SQLError, SQLResult};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub mod entry;

pub trait TableCreationNamespace {
    fn prepare_writer(&self) -> Result<bool, SQLError>;
    fn temporary_name(&self, name: &str) -> Result<String, SQLError>;
    fn persistent_name(&self, name: &str) -> Result<String, SQLError>;
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError>;
}
pub trait TableCreationPublication {
    fn create_table(
        &self,
        name: &str,
        persistence: RelationPersistence,
        on_commit: OnCommitAction,
    ) -> StorageBackendResult<()>;
    fn create_vector_field(
        &self,
        table: &str,
        field: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool>;
    fn install_hierarchy(&self, table: &str, hierarchy: TableHierarchy)
        -> StorageBackendResult<()>;
    fn persist_schema(&self, table: &str) -> StorageBackendResult<bool>;
    fn refresh_value_indexes(&self, table: &str) -> StorageBackendResult<()>;
}
pub struct CreateTableContext<'a> {
    pub namespace: &'a dyn TableCreationNamespace,
    pub analysis: CreateTableAnalysisContext<'a>,
    pub sequences: ImplicitSequenceContext<'a>,
    pub ownership: ImplicitOwnershipContext<'a>,
    pub schema_transactions: &'a dyn SchemaWriteTransaction,
    pub publication: &'a dyn TableCreationPublication,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}
fn storage_error(action: &str, error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}

pub fn run_create_table(
    context: &CreateTableContext<'_>,
    mut table: CreateTable,
) -> Result<SQLResult, SQLError> {
    validate_create_table_columns(&table)?;
    let Some(name) = preflight(context, &table.name, table.persistence, table.if_not_exists)?
    else {
        return Ok(SQLResult::empty());
    };
    table.name = name;
    create_after_preflight(context, table)
}
pub fn run_create_table_if_not_exists(
    context: &CreateTableContext<'_>,
    deferred: DeferredCreateTable,
) -> Result<SQLResult, SQLError> {
    let Some(name) = preflight(context, &deferred.name, deferred.persistence, true)? else {
        return Ok(SQLResult::empty());
    };
    let mut table = uqa_sql::resolve_deferred_create_table(&deferred)?;
    validate_create_table_columns(&table)?;
    table.name = name;
    create_after_preflight(context, table)
}
fn preflight(
    context: &CreateTableContext<'_>,
    name: &str,
    persistence: RelationPersistence,
    if_not_exists: bool,
) -> Result<Option<String>, SQLError> {
    if persistence != RelationPersistence::Temporary {
        context.namespace.prepare_writer()?;
    }
    let name = if persistence == RelationPersistence::Temporary {
        context.namespace.temporary_name(name)?
    } else {
        context.namespace.persistent_name(name)?
    };
    if context.namespace.relation_exists(&name)? {
        let local = uqa_core::RelationIdentity::from_legacy_name(&name)
            .map_err(SQLError::Internal)?
            .name;
        if if_not_exists {
            context.notices.lock().push((
                "NOTICE".into(),
                format!("relation \"{local}\" already exists, skipping"),
            ));
            return Ok(None);
        }
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{local}\" already exists"),
        });
    }
    Ok(Some(name))
}
fn create_after_preflight(
    context: &CreateTableContext<'_>,
    mut table: CreateTable,
) -> Result<SQLResult, SQLError> {
    declaration::prepare_create_table_declaration(&context.analysis, &mut table)?;
    implicit::materialize_implicit_sequences(
        &context.sequences,
        "CREATE TABLE",
        &table.name,
        &mut table.columns,
        table.persistence,
    )?;
    declaration::validate_create_table_expressions(&context.analysis, &mut table)?;
    let mut vector_fields = Vec::new();
    for column in &table.columns {
        match &column.ty {
            ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                vector_fields.push((column.name.clone(), *dim));
            }
            _ => {}
        }
    }
    context
        .publication
        .create_table(&table.name, table.persistence, table.on_commit)
        .map_err(|error| storage_error("CREATE TABLE", error))?;
    for (field, dimensions) in vector_fields {
        context
            .publication
            .create_vector_field(&table.name, field, dimensions)
            .map_err(|error| storage_error("CREATE TABLE vector field", error))?;
    }
    for column in &table.columns {
        context
            .schema_transactions
            .with_schema_write(Box::new(|schema| {
                publication::register_column(
                    schema,
                    &table.name,
                    column.clone(),
                    Some(&table.columns),
                )
            }))
            .map_err(|error| storage_error("CREATE TABLE column", error))?;
    }
    let mut registered_columns = context
        .analysis
        .foreign_keys
        .columns
        .try_describe_table(&table.name)
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("CREATE TABLE columns", error.as_ref())
        })?
        .ok_or_else(|| SQLError::UnknownTable(table.name.clone()))?;
    declaration::bind_created_table_foreign_keys(
        &context.analysis.foreign_keys,
        &mut table,
        &mut registered_columns,
    )?;
    let constraints = TableConstraintSet {
        columns_declared: Some(true),
        persistence: table.persistence,
        on_commit: table.on_commit,
        checks: table.checks.clone(),
        foreign_keys: table.foreign_keys.clone(),
        key_constraints: table.key_constraints.clone(),
        hierarchy: table.hierarchy.clone(),
    };
    context
        .schema_transactions
        .with_schema_write(Box::new(|schema| {
            publication::replace_constraint_state(
                schema,
                &table.name,
                registered_columns,
                constraints,
            )
        }))
        .map_err(|error| storage_error("CREATE TABLE constraints", error))?;
    ownership::attach_table_owners(&context.ownership, &table.name)
        .map_err(|error| storage_error("CREATE TABLE sequence ownership", error))?;
    context
        .publication
        .install_hierarchy(&table.name, table.hierarchy.clone())
        .map_err(|error| storage_error("CREATE TABLE hierarchy", error))?;
    context
        .publication
        .persist_schema(&table.name)
        .map_err(|error| storage_error("CREATE TABLE", error))?;
    context
        .publication
        .refresh_value_indexes(&table.name)
        .map_err(|error| storage_error("CREATE TABLE btree indexes", error))?;
    Ok(SQLResult::empty())
}
