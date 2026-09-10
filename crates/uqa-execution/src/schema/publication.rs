//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schedule schema analysis, durable candidate persistence, and in-memory publication.
use uqa_core::RelationIdentity;
use uqa_sql::ast::{ColumnDef, TableConstraintSet};
use uqa_sql::schema::constraint_metadata::{
    materialize_constraint_metadata, ConstraintMetadataResult,
};
use uqa_sql::schema::dependencies::{
    regclass,
    registration::{self, SchemaDependencyBindingContext},
};
use uqa_sql::type_resolution::FunctionTypeResolver;
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// A retained table generation with only the metadata and writes needed for schema publication.
pub trait TableSchemaState {
    fn columns(&self) -> Vec<ColumnDef>;
    fn write_columns(&self) -> Box<dyn columns::ColumnSchemaWrite + '_>;
    fn persist_columns(&self, columns: &[ColumnDef]) -> StorageBackendResult<()>;
    fn constraint_header(&self) -> TableConstraintSet;
    fn publish_constraints(&self, columns: Vec<ColumnDef>, constraints: TableConstraintSet);

    fn key_constraints(&self) -> Vec<uqa_sql::ast::TableKeyConstraint>;
    fn hierarchy(&self) -> uqa_sql::ast::TableHierarchy;
    fn publish_hierarchy(&self, hierarchy: uqa_sql::ast::TableHierarchy);
    fn constraints(&self) -> TableConstraintSet;
    fn columns_declared(&self) -> bool;
    fn mark_statistics_dirty(&self) -> StorageBackendResult<()>;
    fn persist_candidate(
        &self,
        columns: &[ColumnDef],
        constraints: &TableConstraintSet,
    ) -> StorageBackendResult<()>;
    /// Publish declared columns and CHECK, foreign-key, and key constraints; hierarchy and lifecycle state are published separately.
    fn publish_columns(
        &self,
        columns_declared: bool,
        columns: Vec<ColumnDef>,
        constraints: TableConstraintSet,
    );
    fn persist_next_id(&self) -> StorageBackendResult<()>;
    fn refresh_value_indexes(&self) -> StorageBackendResult<()>;
}

pub trait TableSchemaCatalog {
    fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn table_state(
        &self,
        canonical: &str,
    ) -> StorageBackendResult<Option<Box<dyn TableSchemaState + '_>>>;
}

pub struct SchemaPublicationContext<'a> {
    pub catalog: &'a dyn TableSchemaCatalog,
    pub types: &'a dyn FunctionTypeResolver,
    pub bindings: SchemaDependencyBindingContext<'a>,
    pub allocate_identity: fn(&str) -> ConstraintMetadataResult<[u8; 16]>,
}

fn resolve_table_name(
    catalog: &dyn TableSchemaCatalog,
    name: &str,
) -> StorageBackendResult<String> {
    catalog
        .resolve_table_name(name)?
        .ok_or_else(|| table_not_found(name))
}
fn table_not_found(name: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("table `{name}` does not exist"))
}
fn materialize_metadata(
    context: &SchemaPublicationContext<'_>,
    name: &str,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
) -> StorageBackendResult<bool> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
    let mut allocate = context.allocate_identity;
    materialize_constraint_metadata(&relation, columns, constraints, &mut allocate)
        .map_err(|error| StorageBackendError::Other(error.to_string()))
}

pub fn register_column(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    mut column: ColumnDef,
    check_columns: Option<&[ColumnDef]>,
) -> StorageBackendResult<()> {
    column.ty = uqa_sql::type_resolution::resolve_declared_column_type(context.types, &column.ty)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    let legacy_auto_increment = column
        .auto_increment
        .as_ref()
        .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy);
    let table_name = resolve_table_name(context.catalog, table)?;
    let state = context
        .catalog
        .table_state(&table_name)?
        .ok_or_else(|| table_not_found(&table_name))?;
    if let Some(default) = &mut column.default {
        regclass::bind_sequence_references_in_expr(context.bindings.references, default)
            .map_err(StorageBackendError::Other)?;
    }
    if let Some(generated) = &mut column.generated {
        regclass::bind_sequence_references_in_expr(
            context.bindings.references,
            &mut generated.expression,
        )
        .map_err(StorageBackendError::Other)?;
    }
    if let Some(reference) = &mut column.references {
        reference.table = resolve_table_name(context.catalog, &reference.table)?;
    }
    let mut columns = state.columns();
    uqa_sql::schema::columns::append_registered_column(&table_name, &mut columns, column)
        .map_err(StorageBackendError::Other)?;
    if let Some(check_columns) = check_columns {
        registration::bind_table_schema_routine_identities_with_check_columns(
            &context.bindings,
            &table_name,
            &mut columns,
            &mut [],
            check_columns,
        )
        .map_err(StorageBackendError::Other)?;
    } else {
        registration::bind_table_schema_routine_identities(
            &context.bindings,
            &table_name,
            &mut columns,
            &mut [],
        )
        .map_err(StorageBackendError::Other)?;
    }
    let mut constraints = state.constraints();
    constraints.columns_declared = Some(true);
    materialize_metadata(context, &table_name, &mut columns, &mut constraints)?;
    state.mark_statistics_dirty()?;
    state.persist_candidate(&columns, &constraints)?;
    state.publish_columns(true, columns, constraints);
    if legacy_auto_increment {
        state.persist_next_id()?;
    }
    state.refresh_value_indexes()?;
    Ok(())
}

pub fn replace_constraint_state(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    mut columns: Vec<ColumnDef>,
    mut constraints: TableConstraintSet,
) -> StorageBackendResult<()> {
    let table_name = resolve_table_name(context.catalog, table)?;
    let state = context
        .catalog
        .table_state(&table_name)?
        .ok_or_else(|| table_not_found(&table_name))?;
    constraints.columns_declared = Some(
        constraints
            .columns_declared
            .unwrap_or(state.columns_declared())
            || !columns.is_empty(),
    );
    for column in &mut columns {
        if let Some(reference) = &mut column.references {
            reference.table = resolve_table_name(context.catalog, &reference.table)?;
        }
    }
    for foreign_key in &mut constraints.foreign_keys {
        foreign_key.ref_table = resolve_table_name(context.catalog, &foreign_key.ref_table)?;
    }
    registration::bind_table_schema_routine_identities(
        &context.bindings,
        &table_name,
        &mut columns,
        &mut constraints.checks,
    )
    .map_err(StorageBackendError::Other)?;
    materialize_metadata(context, &table_name, &mut columns, &mut constraints)?;
    state.persist_candidate(&columns, &constraints)?;
    state.publish_columns(
        constraints.columns_declared.unwrap_or(false) || !columns.is_empty(),
        columns,
        constraints,
    );
    state.mark_statistics_dirty()?;
    state.refresh_value_indexes()?;
    Ok(())
}

/// Bind a schema write to the caller's storage transaction and its freshly constructed publication context.
pub type SchemaWrite<'a> =
    Box<dyn FnOnce(&SchemaPublicationContext<'_>) -> StorageBackendResult<()> + 'a>;
pub trait SchemaWriteTransaction {
    fn with_schema_write(&self, write: SchemaWrite<'_>) -> StorageBackendResult<()>;
}

pub mod hierarchy;

pub mod keys;

pub mod columns;
