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
fn described_columns(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    action: &str,
) -> Result<Vec<ColumnDef>, SQLError> {
    context
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error(action, error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
}
/// `ATExecColumnDefault` for `SET DEFAULT`: the column is checked before the default is analyzed against its type. Returns whether a default remains; `SET DEFAULT NULL` leaves none.
pub fn validate_column_default(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    column: &str,
    default: &mut Expr,
) -> Result<bool, SQLError> {
    let columns = described_columns(context, table, "ALTER COLUMN SET DEFAULT")?;
    super::validate_default_change(table, super::altered_column(table, &columns, column)?, true)?;
    let target = context
        .state
        .column_type(table, column)
        .map_err(|error| ddl_storage_error("ALTER COLUMN SET DEFAULT", error))?
        .ok_or_else(|| super::undefined_relation_column(table, column))?;
    let binding = context.bindings.bindings.binding_scope()?;
    crate::schema::defaults::validate_default_expression(
        &crate::schema::SchemaBindingContext {
            catalog: context.bindings.schema,
            binding: &binding.context(),
        },
        default,
        &target,
        column,
    )
}
/// `ATExecColumnDefault` for `DROP DEFAULT`.
pub fn validate_default_removal(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let columns = described_columns(context, table, "ALTER COLUMN DROP DEFAULT")?;
    super::validate_default_change(
        table,
        super::altered_column(table, &columns, column)?,
        false,
    )
}
fn not_generated(table: &str, column: &str) -> Result<String, SQLError> {
    let relation = uqa_core::RelationIdentity::from_legacy_name(table).map_err(|error| {
        SQLError::Internal(format!("resolve ALTER TABLE target `{table}`: {error}"))
    })?;
    Ok(format!(
        "column \"{column}\" of relation \"{}\" is not a generated column",
        relation.name
    ))
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
        .ok_or_else(|| super::missing_altered_column(table, name))?;
    let Some(current) = column.generated.as_ref() else {
        return Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: not_generated(table, name)?,
        });
    };
    let kind = current.kind;
    column.generated = Some(GeneratedColumn {
        kind,
        expression: Box::new(expression),
        function_dependencies: Vec::new(),
    });
    let foreign_keys = context
        .keys
        .try_foreign_keys(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
    let binding = context.bindings.bindings.binding_scope()?;
    crate::schema::generated::prepare_generated_columns(
        &crate::schema::SchemaBindingContext {
            catalog: context.bindings.schema,
            binding: &binding.context(),
        },
        qualifier,
        &mut columns,
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
/// `ATExecDropExpression`'s checks: the notice that skips a column that is not generated under `IF EXISTS`, or `None` to drop the expression of a stored generated column.
pub fn validate_drop_expression(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    name: &str,
    if_exists: bool,
) -> Result<Option<crate::SQLNotice>, SQLError> {
    let columns = described_columns(context, table, "ALTER COLUMN DROP EXPRESSION")?;
    let column = super::altered_column(table, &columns, name)?;
    let Some(generated) = column.generated.as_ref() else {
        let message = not_generated(table, name)?;
        if if_exists {
            return Ok(Some(crate::SQLNotice::notice(format!(
                "{message}, skipping"
            ))));
        }
        return Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message,
        });
    };
    if generated.kind == GeneratedColumnKind::Virtual {
        let relation = uqa_core::RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("resolve ALTER TABLE target `{table}`: {error}"))
        })?;
        return Err(SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: "ALTER TABLE / DROP EXPRESSION is not supported for virtual generated columns"
                .into(),
            detail: Some(format!(
                "Column \"{name}\" of relation \"{}\" is a virtual generated column.",
                relation.name
            )),
            hint: None,
        });
    }
    Ok(None)
}
pub fn analyze_column_type(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    _qualifier: &str,
    name: &str,
    ty: &ColumnType,
) -> Result<Option<GeneratedColumnKind>, SQLError> {
    if !context
        .state
        .has_column(table, name)
        .map_err(|error| ddl_storage_error("ALTER COLUMN", error))?
    {
        return Err(super::missing_altered_column(table, name));
    }
    // ATPrepAlterColumnType requires USAGE on the new type before CheckAttributeType rejects a pseudo-type.
    context.bindings.schema.require_type_usage(ty)?;
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
    Ok(target_generated_kind)
}

/// Rebind the dependent declarations after every column type has changed. A foreign key can refer to another column changed by the same ALTER statement.
pub fn validate_column_type_constraints(
    context: &ColumnAlterAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
) -> Result<(), SQLError> {
    let mut candidate_columns = described_columns(context, table, "ALTER COLUMN TYPE")?;
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
    let binding = context.bindings.bindings.binding_scope()?;
    crate::schema::generated::prepare_generated_columns(
        &crate::schema::SchemaBindingContext {
            catalog: context.bindings.schema,
            binding: &binding.context(),
        },
        qualifier,
        &mut candidate_columns,
        &foreign_keys,
    )?;
    Ok(())
}
