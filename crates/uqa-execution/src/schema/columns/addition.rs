//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute ADD COLUMN in declaration, physical-field, schema, and existing-row order.
use super::{backfill::ColumnBackfillContext, generated::GeneratedRewriteContext};
use crate::schema::publication::SchemaWriteTransaction;
use uqa_core::Value;
use uqa_sql::schema::columns::addition::AddedColumnAnalysisContext;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, GeneratedColumnKind},
    SQLError,
};
use uqa_storage::StorageBackendResult;
pub trait ColumnAdditionNamespace {
    fn ensure_temporary_creation(&self) -> Result<(), SQLError>;
    fn ensure_existing_creation(&self, table: &str) -> Result<(), SQLError>;
}
pub trait ColumnAdditionState {
    fn has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool>;
    fn create_vector_field(
        &self,
        table: &str,
        column: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool>;
    fn add_text_field(&self, table: &str, column: String) -> Result<(), String>;
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
    pub namespace: &'a dyn ColumnAdditionNamespace,
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
    if_not_exists: bool,
) -> Result<(), SQLError> {
    uqa_sql::schema::columns::validate_postgres_column_name(&column.name)?;
    uqa_sql::schema::columns::validate_postgres_relation_column_type(&column.name, &column.ty)?;
    let col_name = column.name.clone();
    if context
        .state
        .has_column(table, &col_name)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
    {
        if if_not_exists {
            return Ok(());
        }
        let relation = uqa_core::RelationIdentity::from_legacy_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve ALTER TABLE target: {error}")))?;
        return Err(SQLError::Routine {
            sqlstate: "42701".into(),
            message: format!(
                "column \"{col_name}\" of relation \"{}\" already exists",
                relation.name
            ),
        });
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
            context.namespace.ensure_temporary_creation()?;
        } else {
            context.namespace.ensure_existing_creation(table)?;
        }
    }
    let generated_kind = column.generated.as_ref().map(|generated| generated.kind);
    match column.ty {
        ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
            context
                .state
                .create_vector_field(table, col_name.clone(), dim)
                .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
        }
        ColumnType::Text if generated_kind != Some(GeneratedColumnKind::Virtual) => {
            if let Err(e) = context.state.add_text_field(table, col_name.clone()) {
                return Err(SQLError::Internal(format!("add_fts_field: {e}")));
            }
        }
        _ => {}
    }
    // Preserve NOT NULL validation while filling existing rows through the stored default and generated-column paths.
    let column_not_null = column.not_null;
    context
        .transactions
        .with_schema_write(Box::new(|publication| {
            super::super::publication::register_column(publication, table, column, None)
        }))
        .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
    initialize_column_rows(context, table, &col_name, generated_kind, column_not_null)?;
    context
        .state
        .persist_schema(table)
        .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
    Ok(())
}

fn initialize_column_rows<S: Clone + 'static>(
    context: &ColumnAdditionContext<'_, S>,
    table: &str,
    col_name: &str,
    generated_kind: Option<GeneratedColumnKind>,
    column_not_null: bool,
) -> Result<(), SQLError> {
    if let Some(kind) = generated_kind {
        super::generated::validate_and_rewrite_generated_rows(
            &context.generated,
            table,
            kind == GeneratedColumnKind::Stored,
        )?;
    } else {
        let default_expr = context
            .analysis
            .foreign_keys
            .columns
            .try_column_insert_default_expr(table, col_name)
            .map_err(|e| {
                uqa_sql::catalog::errors::storage_error(
                    "ALTER TABLE ADD COLUMN default",
                    e.as_ref(),
                )
            })?;
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
