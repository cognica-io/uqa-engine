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
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError>;
}
pub trait TableCreationPublication {
    fn create_table(
        &self,
        name: &str,
        persistence: RelationPersistence,
        on_commit: OnCommitAction,
        owner: &crate::catalog::security::roles::locking::RoleBinding,
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
    pub creation: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub namespace: &'a dyn TableCreationNamespace,
    pub analysis: CreateTableAnalysisContext<'a>,
    pub sequences: ImplicitSequenceContext<'a>,
    pub ownership: ImplicitOwnershipContext<'a>,
    pub schema_transactions: &'a dyn SchemaWriteTransaction,
    pub publication: &'a dyn TableCreationPublication,
    pub notices: &'a crate::query::NoticeQueue,
}
fn storage_error(action: &str, error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}

pub fn run_create_table(
    context: &CreateTableContext<'_>,
    mut table: CreateTable,
) -> Result<SQLResult, SQLError> {
    let owner = context.creation.bind_owner()?;
    // PostgreSQL resolves the creation namespace, analyzes the declaration, validates the row type and only then finds an existing relation.
    let name = creation_name(context, &table.name, table.persistence)?;
    declaration::transform_create_table(&context.analysis, &mut table)?;
    validate_create_table_columns(&table)?;
    if !ensure_new_relation(context, &name, table.if_not_exists)? {
        return Ok(SQLResult::empty());
    }
    table.name = name;
    create_after_preflight(context, table, &owner)
}
pub fn run_create_table_if_not_exists(
    context: &CreateTableContext<'_>,
    deferred: DeferredCreateTable,
) -> Result<SQLResult, SQLError> {
    let owner = context.creation.bind_owner()?;
    let Some(name) = preflight(context, &deferred.name, deferred.persistence, true)? else {
        return Ok(SQLResult::empty());
    };
    let mut table = uqa_sql::resolve_deferred_create_table(&deferred)?;
    declaration::transform_create_table(&context.analysis, &mut table)?;
    validate_create_table_columns(&table)?;
    table.name = name;
    create_after_preflight(context, table, &owner)
}
/// Resolve the new relation's name in its creation namespace, with the authority to create there.
fn creation_name(
    context: &CreateTableContext<'_>,
    name: &str,
    persistence: RelationPersistence,
) -> Result<String, SQLError> {
    if persistence == RelationPersistence::Temporary {
        context.creation.temporary_name(name)
    } else {
        context.namespace.prepare_writer()?;
        context.creation.persistent_relation_name(name)
    }
}
/// Whether the resolved name is free. An existing relation fails, or is skipped with a notice under `IF NOT EXISTS`.
fn ensure_new_relation(
    context: &CreateTableContext<'_>,
    name: &str,
    if_not_exists: bool,
) -> Result<bool, SQLError> {
    if !context.namespace.relation_exists(name)? {
        return Ok(true);
    }
    let local = uqa_core::RelationIdentity::from_legacy_name(name)
        .map_err(SQLError::Internal)?
        .name;
    if if_not_exists {
        context.notices.push(
            uqa_sql::SQLNotice::notice(format!("relation \"{local}\" already exists, skipping"))
                .with_sqlstate("42P07"),
        );
        return Ok(false);
    }
    Err(SQLError::Routine {
        sqlstate: "42P07".into(),
        message: format!("relation \"{local}\" already exists"),
    })
}
fn preflight(
    context: &CreateTableContext<'_>,
    name: &str,
    persistence: RelationPersistence,
    if_not_exists: bool,
) -> Result<Option<String>, SQLError> {
    let name = creation_name(context, name, persistence)?;
    Ok(ensure_new_relation(context, &name, if_not_exists)?.then_some(name))
}
fn create_after_preflight(
    context: &CreateTableContext<'_>,
    mut table: CreateTable,
    owner: &crate::catalog::security::roles::locking::RoleBinding,
) -> Result<SQLResult, SQLError> {
    let inherited_keys =
        declaration::prepare_create_table_declaration(&context.analysis, &mut table)?;
    context.creation.retain_owner(owner)?;
    if preflight(context, &table.name, table.persistence, table.if_not_exists)?.is_none() {
        return Ok(SQLResult::empty());
    }
    implicit::materialize_implicit_sequences(
        &context.sequences,
        "CREATE TABLE",
        &table.name,
        &mut table.columns,
        table.persistence,
    )?;
    declaration::validate_create_table_expressions(&context.analysis, &mut table)?;
    declaration::define_create_table_constraints(&context.analysis, &mut table, &inherited_keys)?;
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
        .create_table(&table.name, table.persistence, table.on_commit, owner)
        .map_err(|error| storage_error("CREATE TABLE", error))?;
    for (field, dimensions) in vector_fields {
        context
            .publication
            .create_vector_field(&table.name, field, dimensions)
            .map_err(|error| storage_error("CREATE TABLE vector field", error))?;
    }
    let mut registered_columns = table.columns.clone();
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
    if let Some(parent) = table
        .hierarchy
        .parents
        .first()
        .filter(|_| table.hierarchy.is_partition())
    {
        // The foreign keys referencing the new partition's ancestors derive constraints on it.
        let parent = parent.clone();
        context
            .schema_transactions
            .with_schema_write(Box::new(move |schema| {
                publication::referenced_partitions::republish_referencing_tables(
                    schema,
                    &parent,
                    crate::row_locks::RelationLockMode::ShareRowExclusive,
                )
                .map_err(|error| {
                    StorageBackendError::backend("CREATE TABLE derived constraints", error)
                })
            }))
            .map_err(|error| storage_error("CREATE TABLE derived constraints", error))?;
    }
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
