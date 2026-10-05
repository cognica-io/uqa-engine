//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! USING expression analysis and assignment compatibility over the original row type.

use crate::ast::{ColumnDef, ColumnType, Expr};
use crate::plan::ExpressionPlan;
use crate::schema::SchemaBindingContext;
use crate::{ColumnIdentity, RowSchema, SQLError, ScalarExpr};

mod assignment;
pub use assignment::{assign_type_transform_value, fold_type_transform_assignment};

pub struct AnalyzedTypeTransform {
    pub plan: ExpressionPlan,
    pub source_type: Option<ColumnType>,
    row_schema: RowSchema,
}

impl AnalyzedTypeTransform {
    #[must_use]
    pub fn row_schema(&self) -> &RowSchema {
        &self.row_schema
    }
}

/// Bind names and types before the ALTER target is validated. The caller retains the original columns for every USING expression of the statement, including columns another subcommand removes or changes.
pub fn analyze_type_transform(
    context: &SchemaBindingContext<'_, '_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &Expr,
) -> Result<AnalyzedTypeTransform, SQLError> {
    let row_schema = original_row_schema(table, qualifier, columns)?;
    let mut plan = ExpressionPlan::lower(expression.clone());
    let source_type = crate::binding::analyze_column_type_transform(
        context.catalog,
        &plan,
        &row_schema,
        context.binding,
    )?;
    crate::binding::bind_expression_plan_routines_for_storage(
        context.catalog,
        &mut plan,
        &[],
        context.binding,
        &row_schema,
    )?;
    Ok(AnalyzedTypeTransform {
        plan,
        source_type,
        row_schema,
    })
}

/// Check assignment coercion only after the target column and type are valid. An unknown literal is read by the target type's input function now; modifiers and domain constraints still apply when the planned expression is assigned to a row.
pub fn coerce_type_transform(
    context: &SchemaBindingContext<'_, '_>,
    column: &str,
    target: &ColumnType,
    transform: &mut AnalyzedTypeTransform,
    explicit_using: bool,
) -> Result<(), SQLError> {
    if transform
        .source_type
        .as_ref()
        .is_some_and(|source| !transform_type_compatible(source, target))
    {
        let ty = target.without_type_modifiers().regtype_name();
        let (message, hint) = if explicit_using {
            (
                format!("result of USING clause for column \"{column}\" cannot be cast automatically to type {ty}"),
                "You might need to add an explicit cast.".into(),
            )
        } else {
            (
                format!("column \"{column}\" cannot be cast automatically to type {ty}"),
                format!(
                    "You might need to specify \"USING {}::{}\".",
                    crate::expr::quote_ident(column),
                    target.regtype_name(),
                ),
            )
        };
        return Err(SQLError::Diagnostic {
            sqlstate: "42804".into(),
            message,
            detail: None,
            hint: Some(hint),
        });
    }
    if transform.source_type.is_none() {
        if let ScalarExpr::Literal(value @ uqa_core::Value::Str(_)) = &transform.plan.scalar {
            let mut expression = Expr::Literal(value.clone());
            crate::schema::defaults::cook_unknown_literal(context, &mut expression, target, false)?;
            transform.plan = ExpressionPlan::lower(expression);
            transform.source_type = crate::binding::bind_expression_plan_routines_for_storage(
                context.catalog,
                &mut transform.plan,
                &[],
                context.binding,
                &transform.row_schema,
            )?;
        }
    }
    Ok(())
}

fn transform_type_compatible(source: &ColumnType, target: &ColumnType) -> bool {
    let embedding = |ty: &ColumnType| matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_));
    // ALTER can replace an embedding declaration and rebuild its indexes. Existing rows still have to satisfy the target carrier shape and dimensions.
    (embedding(source) && embedding(target))
        || crate::type_resolution::assignment_type_compatible(source, target)
}

fn original_row_schema(
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
) -> Result<RowSchema, SQLError> {
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(|error| {
        SQLError::Internal(format!("resolve ALTER TABLE target `{table}`: {error}"))
    })?;
    let schema = RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let canonical = relation.qualified_name();
    let aliases = [qualifier, table, relation.name.as_str(), canonical.as_str()]
        .into_iter()
        .flat_map(|qualifier| {
            columns.iter().enumerate().map(move |(index, column)| {
                (ColumnIdentity::qualified(qualifier, &column.name), index)
            })
        })
        .collect::<Vec<_>>();
    Ok(RowSchema::with_identity_aliases(&schema, &aliases))
}

#[cfg(test)]
mod tests;
