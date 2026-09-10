//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze altered column expressions and types against ordered catalog snapshots.
use crate::{
    assignment::columns::{AssignmentColumnCatalog, ColumnCatalogError},
    ast::{ColumnDef, ColumnType, Expr, GeneratedColumn, GeneratedColumnKind},
    schema::{
        columns::addition::AddedColumnKeys, constraint_changes::ConstraintTypeContext,
        dependencies::registration::SchemaDependencyBindingContext,
    },
    SQLError,
};
/// Read only the declared type, existence, or loaded column generation requested by an ALTER action.
pub trait ColumnChangeCatalog {
    fn has_column(&self, table: &str, column: &str) -> Result<bool, ColumnCatalogError>;
    fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<ColumnType>, ColumnCatalogError>;
    fn stored_columns(&self, table: &str) -> Result<Vec<ColumnDef>, ColumnCatalogError>;
}
pub struct ColumnAlterAnalysisContext<'a> {
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub keys: &'a dyn AddedColumnKeys,
    pub state: &'a dyn ColumnChangeCatalog,
    pub bindings: SchemaDependencyBindingContext<'a>,
    pub constraint_types: ConstraintTypeContext<'a>,
}
fn ddl_storage_error(action: &str, error: ColumnCatalogError) -> SQLError {
    crate::catalog::errors::storage_error(action, error.as_ref())
}
pub fn generated_columns_referencing_column(columns: &[ColumnDef], column: &str) -> Vec<String> {
    columns
        .iter()
        .filter(|candidate| candidate.name != column)
        .filter(|candidate| {
            candidate.generated.as_ref().is_some_and(|generated| {
                crate::schema::dependencies::schema_expr_references_column(
                    &generated.expression,
                    column,
                )
            })
        })
        .map(|candidate| candidate.name.clone())
        .collect()
}
pub fn validate_column_default(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    column: &str,
    default: &mut Expr,
) -> Result<(), SQLError> {
    super::reject_default_change_on_generated_column(context.columns, table, column)?;
    let target = context
        .state
        .column_type(table, column)
        .map_err(|error| ddl_storage_error("ALTER COLUMN SET DEFAULT", error))?
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{column}")))?;
    let binding = context.bindings.bindings.binding_scope()?;
    crate::schema::defaults::validate_default_expression(
        &crate::schema::SchemaBindingContext {
            catalog: context.bindings.schema,
            binding: &binding.context(),
        },
        default,
        &target,
    )
}
pub fn analyze_generated_expression(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
    name: &str,
    expression: Expr,
) -> Result<(GeneratedColumn, GeneratedColumnKind), SQLError> {
    let mut columns = context
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let column = columns
        .iter_mut()
        .find(|column| column.name == name)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{name}")))?;
    let Some(current) = column.generated.as_ref() else {
        return Err(SQLError::TypeMismatch(format!(
            "column `{name}` of relation `{table}` is not a generated column"
        )));
    };
    let kind = current.kind;
    column.generated = Some(GeneratedColumn {
        kind,
        expression: Box::new(expression),
        function_dependencies: Vec::new(),
    });
    let key_constraints = context
        .keys
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
    let foreign_keys = context
        .keys
        .try_foreign_keys(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
    crate::schema::generated::prepare_generated_columns(
        context.bindings.schema,
        qualifier,
        &mut columns,
        &key_constraints,
        &foreign_keys,
    )?;
    let generated = columns
        .iter()
        .find(|column| column.name == name)
        .and_then(|column| column.generated.clone())
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "generated column `{name}` disappeared during validation"
            ))
        })?;
    Ok((generated, kind))
}
pub fn validate_drop_expression(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    let columns = context
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let column = columns
        .iter()
        .find(|column| column.name == name)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{name}")))?;
    let Some(generated) = column.generated.as_ref() else {
        return Err(SQLError::TypeMismatch(format!(
            "column `{name}` of relation `{table}` is not a generated column"
        )));
    };
    if generated.kind == GeneratedColumnKind::Virtual {
        return Err(SQLError::Unsupported(format!(
            "ALTER TABLE / DROP EXPRESSION is not supported for virtual generated column `{name}`"
        )));
    }
    Ok(())
}
pub fn analyze_column_type(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
    name: &str,
    ty: &ColumnType,
) -> Result<Option<GeneratedColumnKind>, SQLError> {
    if !context
        .state
        .has_column(table, name)
        .map_err(|error| ddl_storage_error("ALTER COLUMN", error))?
    {
        return Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
        )));
    }
    super::validate_postgres_relation_column_type(name, ty)?;
    let mut candidate_columns = context
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let candidate = candidate_columns
        .iter_mut()
        .find(|column| column.name == name)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{name}")))?;
    candidate.ty.clone_from(ty);
    let target_generated_kind = candidate.generated.as_ref().map(|generated| generated.kind);
    if target_generated_kind.is_none() {
        let columns = context
            .state
            .stored_columns(table)
            .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
        let dependents = generated_columns_referencing_column(&columns, name);
        if !dependents.is_empty() {
            return Err(SQLError::TypeMismatch(format!("cannot alter type of column `{name}` because generated column(s) `{}` depend on it", dependents.join("`, `"))));
        }
    }
    let key_constraints = context
        .keys
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
    let foreign_keys = context
        .keys
        .try_foreign_keys(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
    crate::schema::constraint_changes::validate_altered_constraint_column_types(
        &context.constraint_types,
        table,
        &candidate_columns,
        &key_constraints,
        &foreign_keys,
    )?;
    crate::schema::generated::prepare_generated_columns(
        context.bindings.schema,
        qualifier,
        &mut candidate_columns,
        &key_constraints,
        &foreign_keys,
    )?;
    Ok(target_generated_kind)
}
