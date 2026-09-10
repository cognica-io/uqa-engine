//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind, persist, and publish declared columns while retaining the original column write lock.
use super::{materialize_metadata, resolve_table_name, table_not_found, SchemaPublicationContext};
use uqa_sql::ast::{
    ColumnDef, ColumnType, Expr, ForeignKey, GeneratedColumn, TableCheck, TableKeyConstraint,
};
use uqa_sql::schema::columns::publication::{self, ColumnProperty};
use uqa_sql::schema::dependencies::{regclass, registration};
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// A retained column write guard; candidate persistence runs before its value is replaced.
pub trait ColumnSchemaWrite {
    fn columns(&self) -> &[ColumnDef];
    fn publish(&mut self, columns: Vec<ColumnDef>);
}

pub fn set_column_default(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    column: &str,
    default: Option<Expr>,
) -> StorageBackendResult<bool> {
    set_column_property(context, table, column, ColumnProperty::Default(default))
}
pub fn set_column_generated(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    column: &str,
    generated: Option<GeneratedColumn>,
) -> StorageBackendResult<bool> {
    set_column_property(context, table, column, ColumnProperty::Generated(generated))
}
pub fn set_column_type(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    column: &str,
    ty: &ColumnType,
) -> StorageBackendResult<bool> {
    set_column_property(context, table, column, ColumnProperty::Type(ty))
}
fn set_column_property(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    column: &str,
    mut property: ColumnProperty<'_>,
) -> StorageBackendResult<bool> {
    let table_name = resolve_table_name(context.catalog, table)?;
    let state = context
        .catalog
        .table_state(&table_name)?
        .ok_or_else(|| table_not_found(&table_name))?;
    match &mut property {
        ColumnProperty::Default(Some(default)) => {
            regclass::bind_sequence_references_in_expr(context.bindings.references, default)
                .map_err(StorageBackendError::Other)?;
            registration::bind_default_routine_identities(
                &context.bindings,
                &table_name,
                column,
                default,
            )
            .map_err(StorageBackendError::Other)?;
        }
        ColumnProperty::Generated(Some(generated)) => {
            regclass::bind_sequence_references_in_expr(
                context.bindings.references,
                &mut generated.expression,
            )
            .map_err(StorageBackendError::Other)?;
        }
        _ => {}
    }
    let mut guard = state.write_columns();
    let mut next = guard.columns().to_vec();
    publication::apply_property(&mut next, &table_name, column, property)
        .map_err(StorageBackendError::Other)?;
    state.mark_statistics_dirty()?;
    state.persist_columns(&next)?;
    guard.publish(next);
    Ok(true)
}
pub fn set_column_not_null(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    column: &str,
    not_null: bool,
) -> StorageBackendResult<bool> {
    let table_name = resolve_table_name(context.catalog, table)?;
    let state = context
        .catalog
        .table_state(&table_name)?
        .ok_or_else(|| table_not_found(&table_name))?;
    let mut next = state.columns();
    publication::set_not_null(&mut next, &table_name, column, not_null)
        .map_err(StorageBackendError::Other)?;
    let mut constraints = state.constraints();
    materialize_metadata(context, &table_name, &mut next, &mut constraints)?;
    state.mark_statistics_dirty()?;
    state.persist_candidate(&next, &constraints)?;
    state.publish_constraints(next, constraints);
    Ok(true)
}
pub fn register_table_constraints(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    checks: Vec<TableCheck>,
    mut foreign_keys: Vec<ForeignKey>,
    key_constraints: Vec<TableKeyConstraint>,
) -> StorageBackendResult<()> {
    let Some(table_name) = context.catalog.resolve_table_name(table)? else {
        return Err(StorageBackendError::Other(format!(
            "unknown table `{table}` while registering constraints"
        )));
    };
    let Some(state) = context.catalog.table_state(&table_name)? else {
        return Err(StorageBackendError::Other(format!(
            "unknown table `{table_name}` while registering constraints"
        )));
    };
    for foreign_key in &mut foreign_keys {
        foreign_key.ref_table = resolve_table_name(context.catalog, &foreign_key.ref_table)?;
    }
    let mut constraints = state.constraint_header();
    constraints.checks = checks;
    constraints.foreign_keys = foreign_keys;
    constraints.key_constraints = key_constraints;
    // Validate the stored relation identity before taking the declared column snapshot.
    let relation = uqa_core::RelationIdentity::from_legacy_name(&table_name)
        .map_err(StorageBackendError::Other)?;
    let mut columns = state.columns();
    registration::bind_table_schema_routine_identities(
        &context.bindings,
        &table_name,
        &mut columns,
        &mut constraints.checks,
    )
    .map_err(StorageBackendError::Other)?;
    let mut allocate = context.allocate_identity;
    uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
        &relation,
        &mut columns,
        &mut constraints,
        &mut allocate,
    )
    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    state.persist_candidate(&columns, &constraints)?;
    state.publish_constraints(columns, constraints);
    Ok(())
}
