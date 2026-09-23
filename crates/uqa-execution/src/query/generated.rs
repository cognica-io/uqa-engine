//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical/logical row conversion for `PostgreSQL` generated columns.

use uqa_core::{
    memory::{MemoryBudget, MemoryReservation},
    CancellationToken,
};
use uqa_sql::ast::{ColumnDef, GeneratedColumnKind};
use uqa_sql::{
    expr::RowLookup,
    schema::{ColumnTypeSchema, ScalarTypeSchema},
    SQLError,
};
use uqa_storage::document_store::Document;

pub fn materialize_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |_| true)
}

pub fn materialize_selected_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    selected: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |name| {
        selected.contains(name)
    })
}

pub fn materialize_projected_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    projection: &[String],
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |name| {
        projection.iter().any(|projected| projected == name)
    })
}

pub fn materialize_missing_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
) -> Result<(), SQLError> {
    materialize_matching_missing_generated_columns(columns, document, |_| true, None)
}

/// Resource scopes for AST-to-IR production. Type binding and evaluated values retain their separate allocation contracts; schema lookup borrows the existing column owner.
pub(crate) struct GeneratedLoweringControl<'a> {
    pub(crate) budget: &'a MemoryBudget,
    pub(crate) original: &'a CancellationToken,
    pub(crate) invoking: &'a CancellationToken,
}

pub(crate) fn materialize_missing_generated_columns_with_lowering_control(
    columns: &[ColumnDef],
    document: &mut Document,
    control: &GeneratedLoweringControl<'_>,
) -> Result<(), SQLError> {
    materialize_matching_missing_generated_columns(columns, document, |_| true, Some(control))
}

fn materialize_matching_missing_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    mut selected: impl FnMut(&str) -> bool,
    lowering: Option<&GeneratedLoweringControl<'_>>,
) -> Result<(), SQLError> {
    let schema = ColumnTypeSchema::new(columns);
    for column in columns {
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if document.contains_key(&column.name) || !selected(&column.name) {
            continue;
        }
        let value = if let Some(control) = lowering {
            let expression =
                prepare_generated_column_with_lowering_control(&schema, generated, control)?;
            evaluate_generated_expression(&expression.scalar, document)?
        } else {
            evaluate_generated_column(&schema, generated, document)?
        };
        document.insert(
            column.name.clone(),
            uqa_sql::assignment::conversion::convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}

fn materialize_matching_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    mut selected: impl FnMut(&str) -> bool,
) -> Result<(), SQLError> {
    let schema = ColumnTypeSchema::new(columns);
    for column in columns {
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if generated.kind != GeneratedColumnKind::Virtual {
            continue;
        }
        if !selected(&column.name) {
            continue;
        }
        let value = evaluate_generated_column(&schema, generated, document)?;
        document.insert(
            column.name.clone(),
            uqa_sql::assignment::conversion::convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}

fn evaluate_generated_column(
    schema: &dyn ScalarTypeSchema,
    generated: &uqa_sql::ast::GeneratedColumn,
    document: &Document,
) -> Result<uqa_core::Value, SQLError> {
    let expression = prepare_generated_column(schema, generated)?;
    evaluate_generated_expression(&expression, document)
}

pub(crate) fn prepare_generated_column(
    schema: &dyn ScalarTypeSchema,
    generated: &uqa_sql::ast::GeneratedColumn,
) -> Result<crate::ScalarExpr, SQLError> {
    let mut expression = uqa_sql::plan::ExpressionPlan::lower((*generated.expression).clone());
    if !expression.subqueries.is_empty() {
        return Err(SQLError::Internal(
            "validated generated expression contains a subquery".into(),
        ));
    }
    expression.scalar = crate::bind_type_introspection(expression.scalar, schema, &[]);
    Ok(expression.scalar)
}

/// Keep the lowering lease through binding and evaluation without claiming ownership of allocations subsequently produced by those owners.
pub(crate) struct GeneratedExpression {
    pub(crate) scalar: crate::ScalarExpr,
    _lowering_memory: MemoryReservation,
}

pub(crate) fn prepare_generated_column_with_lowering_control(
    schema: &dyn ScalarTypeSchema,
    generated: &uqa_sql::ast::GeneratedColumn,
    control: &GeneratedLoweringControl<'_>,
) -> Result<GeneratedExpression, SQLError> {
    let lowered = uqa_sql::plan::ExpressionPlan::lower_column_budgeted(
        &generated.expression,
        control.budget,
        control.original,
        control.invoking,
    )?;
    let (scalar, memory) = lowered.into_parts();
    let scalar = crate::bind_type_introspection(scalar, schema, &[]);
    control.original.check()?;
    control.invoking.check()?;
    Ok(GeneratedExpression {
        scalar,
        _lowering_memory: memory,
    })
}

pub(crate) fn evaluate_generated_expression(
    expression: &crate::ScalarExpr,
    row: &dyn RowLookup,
) -> Result<uqa_core::Value, SQLError> {
    crate::eval_scalar(
        expression,
        &crate::ScalarEvalContext::from_row_lookup(row, &[]),
    )
}

pub fn strip_virtual_generated_columns(columns: &[ColumnDef], document: &mut Document) {
    for column in columns {
        if column
            .generated
            .as_ref()
            .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
        {
            document.remove(&column.name);
        }
    }
}

pub fn projection_contains_virtual_generated_column(
    columns: &[ColumnDef],
    projection: &[String],
) -> bool {
    columns.iter().any(|column| {
        column
            .generated
            .as_ref()
            .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
            && projection.iter().any(|name| name == &column.name)
    })
}

#[cfg(test)]
mod tests;
