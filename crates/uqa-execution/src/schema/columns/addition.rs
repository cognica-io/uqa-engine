//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish the declared column before its physical fields and validate backfilled keys atomically.
use super::{backfill::ColumnBackfillContext, generated::GeneratedRewriteContext};
use crate::schema::publication::SchemaWriteTransaction;
use uqa_core::Value;
use uqa_sql::schema::columns::addition::AddedColumnAnalysisContext;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, GeneratedColumnKind},
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub trait ColumnAdditionState {
    fn has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool>;
    fn create_vector_field(
        &self,
        table: &str,
        column: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool>;
    fn add_text_field(&self, table: &str, column: String) -> Result<(), SQLError>;
    fn set_missing_value(
        &self,
        table: &str,
        column: &str,
        value: Option<Value>,
    ) -> Result<(), SQLError>;
    fn persist_schema(&self, table: &str) -> StorageBackendResult<bool>;
}
pub struct ColumnAdditionContext<'a, S: Clone + 'static> {
    pub analysis: AddedColumnAnalysisContext<'a>,
    pub namespace: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub sequences: crate::schema::sequences::implicit::ImplicitSequenceContext<'a>,
    pub ownership: crate::schema::sequences::ownership::ImplicitOwnershipContext<'a>,
    pub state: &'a dyn ColumnAdditionState,
    pub transactions: &'a dyn SchemaWriteTransaction,
    pub generated: GeneratedRewriteContext<'a, S>,
    pub backfill: ColumnBackfillContext<'a>,
}
fn ddl_storage_error(action: &str, error: uqa_storage::StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
pub fn add_column<S: Clone + 'static>(
    context: &ColumnAdditionContext<'_, S>,
    table: &str,
    qualifier: &str,
    mut column: ColumnDef,
    key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    if_not_exists: bool,
) -> Result<(), SQLError> {
    uqa_sql::schema::columns::validate_postgres_column_name(&column.name)?;
    uqa_sql::schema::columns::validate_postgres_relation_column_type(&column.name, &column.ty)?;
    let col_name = column.name.clone();
    if column_exists(context, table, &col_name, if_not_exists)? {
        return Ok(());
    }
    uqa_sql::schema::columns::addition::bind_added_column(
        &context.analysis,
        table,
        qualifier,
        &mut column,
    )?;
    if column.primary_key || column.unique {
        let persistence = context
            .generated
            .keys
            .catalog
            .table_persistence(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN constraint", error))?;
        if persistence == Some(uqa_sql::ast::RelationPersistence::Temporary) {
            context.namespace.ensure_temporary_privilege()?;
        } else {
            context.namespace.ensure_existing_create(table)?;
        }
    }
    let generated_kind = column.generated.as_ref().map(|generated| generated.kind);
    let column_type = column.ty.clone();
    let adds_keys = column.primary_key || column.unique || !key_constraints.is_empty();
    // Preserve NOT NULL validation while filling existing rows through the stored default and generated-column paths.
    let column_not_null = column.not_null;
    let identity_sequence = column
        .auto_increment
        .as_ref()
        .filter(|provenance| provenance.is_identity())
        .and_then(|provenance| provenance.sequence.clone());
    let owns_sequence = column
        .auto_increment
        .as_ref()
        .is_some_and(|provenance| provenance.sequence.is_some());
    context
        .transactions
        .with_schema_write(Box::new(|publication| {
            super::super::publication::register_column(
                publication,
                table,
                column,
                None,
                key_constraints,
            )
        }))
        .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
    if owns_sequence {
        crate::schema::sequences::ownership::attach_table_owners(&context.ownership, table)
            .map_err(|error| {
                ddl_storage_error("ALTER TABLE ADD COLUMN sequence ownership", error)
            })?;
    }
    // Constraint name reservations can refresh the catalog. Physical field metadata must already have a declared column whenever that happens.
    match column_type {
        ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
            context
                .state
                .create_vector_field(table, col_name.clone(), dim)
                .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
        }
        ColumnType::Text if generated_kind != Some(GeneratedColumnKind::Virtual) => {
            context.state.add_text_field(table, col_name.clone())?;
        }
        _ => {}
    }
    initialize_column_rows(
        context,
        table,
        &col_name,
        generated_kind,
        identity_sequence.as_deref(),
        column_not_null,
    )?;
    if adds_keys {
        for constraint in context
            .generated
            .keys
            .catalog
            .try_key_constraints(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN keys", error))?
        {
            if constraint.columns.contains(&col_name) {
                super::super::keys::validate_key_constraint_data(
                    &context.generated.keys,
                    table,
                    &constraint,
                )?;
            }
        }
    }
    context
        .state
        .persist_schema(table)
        .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
    Ok(())
}

/// Whether `table` already has the column `col_name`, which `IF NOT EXISTS` skips and an addition without it fails on.
fn column_exists<S: Clone + 'static>(
    context: &ColumnAdditionContext<'_, S>,
    table: &str,
    col_name: &str,
    if_not_exists: bool,
) -> Result<bool, SQLError> {
    if !context
        .state
        .has_column(table, col_name)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
    {
        return Ok(false);
    }
    if if_not_exists {
        return Ok(true);
    }
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve ALTER TABLE target: {error}")))?;
    Err(SQLError::Routine {
        sqlstate: "42701".into(),
        message: format!(
            "column \"{col_name}\" of relation \"{}\" already exists",
            relation.name
        ),
    })
}

/// Create the sequence of a `SERIAL` or identity column added to `table`, which exists before the column does. An existing column fails first, so no sequence is created for it.
pub fn create_added_column_sequence<S: Clone + 'static>(
    context: &ColumnAdditionContext<'_, S>,
    table: &str,
    column: &mut ColumnDef,
) -> Result<(), SQLError> {
    if column
        .auto_increment
        .as_ref()
        .is_none_or(|provenance| provenance.sequence.is_some())
    {
        return Ok(());
    }
    column_exists(context, table, &column.name, false)?;
    let persistence = context
        .generated
        .keys
        .catalog
        .table_persistence(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN sequence", error))?
        .unwrap_or_default();
    crate::schema::sequences::implicit::materialize_implicit_sequences(
        &context.sequences,
        "ALTER TABLE",
        table,
        std::slice::from_mut(column),
        persistence,
    )
}

/// Fill the new column of every existing row: from its generation expression, the next value of an identity column's sequence, or its default.
fn initialize_column_rows<S: Clone + 'static>(
    context: &ColumnAdditionContext<'_, S>,
    table: &str,
    col_name: &str,
    generated_kind: Option<GeneratedColumnKind>,
    identity_sequence: Option<&str>,
    column_not_null: bool,
) -> Result<(), SQLError> {
    if let Some(kind) = generated_kind {
        super::generated::validate_and_rewrite_generated_rows(
            &context.generated,
            table,
            kind == GeneratedColumnKind::Stored,
        )?;
    } else {
        let default_expr = match identity_sequence {
            Some(sequence) => Some(uqa_sql::schema::sequences::implicit::sequence_next_value(
                sequence,
            )),
            None => context
                .analysis
                .foreign_keys
                .columns
                .try_column_insert_default_expr(table, col_name)
                .map_err(|e| {
                    uqa_sql::catalog::errors::storage_error(
                        "ALTER TABLE ADD COLUMN default",
                        e.as_ref(),
                    )
                })?,
        };
        let missing_value = super::backfill::backfill_added_column(
            &context.backfill,
            table,
            col_name,
            default_expr.as_ref(),
            column_not_null,
        )?;
        context
            .state
            .set_missing_value(table, col_name, missing_value)?;
    }
    Ok(())
}
