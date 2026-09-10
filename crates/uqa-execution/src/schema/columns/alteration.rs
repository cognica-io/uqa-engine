//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute default, generated-expression, and type changes through schema transactions and row rewrites.
use super::{
    addition::ColumnAdditionState, generated::GeneratedRewriteContext, ColumnRewriteContext,
};
use crate::schema::publication::{columns as publication, SchemaWriteTransaction};
use uqa_sql::{
    ast::{ColumnType, Expr, GeneratedColumnKind},
    schema::columns::{
        alteration::{self, ColumnAlterAnalysisContext},
        publication::ColumnProperty,
    },
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};
/// Physical field index lifecycle used when a column changes storage type.
pub trait ColumnIndexChanges {
    fn drop_vector_indexes(&self, table: &str, column: &str) -> StorageBackendResult<bool>;
    fn rebuild_vector_index(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool>;
}
pub struct ColumnAlterContext<'a, S: Clone + 'static> {
    pub analysis: ColumnAlterAnalysisContext<'a>,
    pub fields: &'a dyn ColumnAdditionState,
    pub indexes: &'a dyn ColumnIndexChanges,
    pub transactions: &'a dyn SchemaWriteTransaction,
    pub generated: GeneratedRewriteContext<'a, S>,
    pub rewrite: ColumnRewriteContext<'a>,
}
fn ddl_storage_error(action: &str, error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
fn publish_property(
    transactions: &dyn SchemaWriteTransaction,
    table: &str,
    column: &str,
    property: ColumnProperty<'_>,
) -> StorageBackendResult<bool> {
    let mut changed = false;
    transactions.with_schema_write(Box::new(|context| {
        changed = match property {
            ColumnProperty::Default(default) => {
                publication::set_column_default(context, table, column, default)?
            }
            ColumnProperty::Generated(generated) => {
                publication::set_column_generated(context, table, column, generated)?
            }
            ColumnProperty::Type(ty) => publication::set_column_type(context, table, column, ty)?,
        };
        Ok(())
    }))?;
    Ok(changed)
}
pub fn set_default<S: Clone + 'static>(
    context: &ColumnAlterContext<'_, S>,
    table: &str,
    name: &str,
    mut default: Expr,
) -> Result<(), SQLError> {
    alteration::validate_column_default(&context.analysis, table, name, &mut default)?;
    if !publish_property(
        context.transactions,
        table,
        name,
        ColumnProperty::Default(Some(default)),
    )
    .map_err(|error| ddl_storage_error("ALTER COLUMN SET DEFAULT", error))?
    {
        return Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
        )));
    }
    context
        .fields
        .persist_schema(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ALTER COLUMN", error))?;
    Ok(())
}
pub fn drop_default<S: Clone + 'static>(
    context: &ColumnAlterContext<'_, S>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    uqa_sql::schema::columns::reject_default_change_on_generated_column(
        context.analysis.columns,
        table,
        name,
    )?;
    if !publish_property(
        context.transactions,
        table,
        name,
        ColumnProperty::Default(None),
    )
    .map_err(|error| ddl_storage_error("ALTER COLUMN DROP DEFAULT", error))?
    {
        return Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
        )));
    }
    context
        .fields
        .persist_schema(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ALTER COLUMN", error))?;
    Ok(())
}
pub fn set_expression<S: Clone + 'static>(
    context: &ColumnAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    name: &str,
    expression: Expr,
) -> Result<(), SQLError> {
    let (generated, kind) = alteration::analyze_generated_expression(
        &context.analysis,
        table,
        qualifier,
        name,
        expression,
    )?;
    publish_property(
        context.transactions,
        table,
        name,
        ColumnProperty::Generated(Some(generated)),
    )
    .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
    super::generated::validate_and_rewrite_generated_rows(
        &context.generated,
        table,
        kind == GeneratedColumnKind::Stored,
    )
}
pub fn drop_expression<S: Clone + 'static>(
    context: &ColumnAlterContext<'_, S>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    alteration::validate_drop_expression(&context.analysis, table, name)?;
    publish_property(
        context.transactions,
        table,
        name,
        ColumnProperty::Generated(None),
    )
    .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?;
    Ok(())
}
pub fn alter_type<S: Clone + 'static>(
    context: &ColumnAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    name: &str,
    ty: &ColumnType,
    using: Option<&Expr>,
) -> Result<(), SQLError> {
    let target_generated_kind =
        alteration::analyze_column_type(&context.analysis, table, qualifier, name, ty)?;
    let old_ty = context
        .analysis
        .state
        .column_type(table, name)
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("ALTER COLUMN TYPE", error.as_ref())
        })?
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{name}")))?;
    let old_was_vector = matches!(&old_ty, ColumnType::Vector(_) | ColumnType::Tensor(_));
    let new_is_vector = matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_));
    // Detach vector indexes before scalar conversion so row publication cannot feed a scalar into the old index; the enclosing transaction restores physical and catalog state on failure.
    if old_was_vector && !new_is_vector {
        context
            .indexes
            .drop_vector_indexes(table, name)
            .map_err(|error| ddl_storage_error("ALTER TABLE ALTER COLUMN", error))?;
    }
    if target_generated_kind.is_none() {
        super::rewrite_column_values_to_type(&context.rewrite, table, name, &old_ty, ty, using)?;
    }
    publish_property(context.transactions, table, name, ColumnProperty::Type(ty))
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
    match ty {
        ColumnType::Text if target_generated_kind != Some(GeneratedColumnKind::Virtual) => {
            context
                .fields
                .add_text_field(table, name.to_string())
                .map_err(|error| SQLError::Internal(format!("add_fts_field: {error}")))?;
        }
        ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions) => {
            context
                .indexes
                .rebuild_vector_index(table, name, *dimensions)
                .map_err(|error| ddl_storage_error("ALTER TABLE ALTER COLUMN", error))?;
        }
        _ => {}
    }
    if let Some(kind) = target_generated_kind {
        super::generated::validate_and_rewrite_generated_rows(
            &context.generated,
            table,
            kind == GeneratedColumnKind::Stored,
        )?;
    }
    super::generated::validate_all_table_rows(
        context.generated.state,
        context.generated.keys.constraints,
    )?;
    context
        .fields
        .persist_schema(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ALTER COLUMN", error))?;
    Ok(())
}
