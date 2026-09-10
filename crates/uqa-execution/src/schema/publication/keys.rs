//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist and publish one validated key without replacing unrelated constraints.
use super::{materialize_metadata, resolve_table_name, table_not_found, SchemaPublicationContext};
use uqa_sql::ast::TableKeyConstraint;
use uqa_storage::{StorageBackendError, StorageBackendResult};
pub fn append_key_constraint(
    context: &SchemaPublicationContext<'_>,
    table: &str,
    constraint: &TableKeyConstraint,
) -> StorageBackendResult<()> {
    let table_name = resolve_table_name(context.catalog, table)?;
    let state = context
        .catalog
        .table_state(&table_name)?
        .ok_or_else(|| table_not_found(&table_name))?;
    let mut key_constraints = state.key_constraints();
    key_constraints.push(constraint.clone());
    let mut columns = state.columns();
    uqa_sql::schema::keys::apply_primary_key_columns(&table_name, constraint, &mut columns)
        .map_err(StorageBackendError::Other)?;
    let mut constraints = state.constraints();
    constraints.key_constraints = key_constraints;
    materialize_metadata(context, &table_name, &mut columns, &mut constraints)?;
    state.persist_candidate(&columns, &constraints)?;
    state.publish_constraints(columns, constraints);
    state.refresh_value_indexes()?;
    Ok(())
}
