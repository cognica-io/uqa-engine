//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical/logical row conversion for `PostgreSQL` generated columns.

use uqa_core::{
    memory::{MemoryBudget, MemoryReservation, Produced, ProductionControl},
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
    materialize_matching_missing_generated_columns(columns, document, None)
}

/// The original allowance and cancellation scopes follow generated expression preparation and value production; schema and row lookups borrow their existing owners.
pub(crate) struct GeneratedControl<'a> {
    pub(crate) budget: &'a MemoryBudget,
    pub(crate) original: &'a CancellationToken,
    pub(crate) invoking: &'a CancellationToken,
}

impl GeneratedControl<'_> {
    fn production(&self) -> ProductionControl<'_> {
        ProductionControl::new(self.budget, self.original, self.invoking)
    }
}

pub(crate) fn materialize_missing_generated_columns_with_control(
    columns: &[ColumnDef],
    document: &mut Document,
    memory: &mut MemoryReservation,
    control: &GeneratedControl<'_>,
) -> Result<(), SQLError> {
    assert!(control.budget.shares_allowance(memory.budget()));
    materialize_matching_missing_generated_columns(columns, document, Some((control, memory)))
}

fn materialize_matching_missing_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    mut retained: Option<(&GeneratedControl<'_>, &mut MemoryReservation)>,
) -> Result<(), SQLError> {
    let production = retained
        .as_ref()
        .map_or_else(ProductionControl::uncontrolled, |(control, _)| {
            control.production()
        });
    production.check()?;
    let schema = ColumnTypeSchema::new(columns);
    for column in columns {
        production.check()?;
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if document.contains_key(&column.name) {
            continue;
        }
        let value = if let Some((control, _)) = &retained {
            let expression = prepare_generated_column_with_control(&schema, generated, control)?;
            evaluate_generated_expression_with_control(&expression.scalar, document, &production)?
        } else {
            production.finish(
                evaluate_generated_column(&schema, generated, document)?,
                None,
            )?
        };
        let value = uqa_sql::assignment::conversion::convert_value_to_column_type_with_control(
            value,
            &column.ty,
            &production,
        )?;
        let name = production.copy_text(&column.name)?;
        let entry_memory = production.reserve(size_of::<(String, uqa_core::Value)>())?;
        let (value, value_memory) = value.into_parts();
        let (name, name_memory) = name.into_parts();
        let entry = production.finish(
            (name, value),
            production.combine(entry_memory, production.combine(value_memory, name_memory)),
        )?;
        let ((name, value), entry_memory) = entry.into_parts();
        match (&mut retained, entry_memory) {
            (Some((_, memory)), Some(entry_memory)) => memory.absorb(entry_memory),
            (None, None) => (),
            _ => unreachable!("generated field ownership matches its production control"),
        }
        document.insert(name, value);
    }
    production.check()?;
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

/// Keep every lowered and bound expression allocation through evaluation; evaluated values have their own result owner.
pub(crate) struct GeneratedExpression {
    pub(crate) scalar: crate::ScalarExpr,
    _preparation_memory: MemoryReservation,
}

pub(crate) fn prepare_generated_column_with_control(
    schema: &dyn ScalarTypeSchema,
    generated: &uqa_sql::ast::GeneratedColumn,
    control: &GeneratedControl<'_>,
) -> Result<GeneratedExpression, SQLError> {
    let lowered = uqa_sql::plan::ExpressionPlan::lower_column_budgeted(
        &generated.expression,
        control.budget,
        control.original,
        control.invoking,
    )?;
    let prepared = uqa_sql::bind_type_introspection_with_control(
        lowered.into(),
        schema,
        &[],
        &ProductionControl::new(control.budget, control.original, control.invoking),
    )?
    .into_budgeted()
    .expect("controlled generated preparation retains its allowance");
    let (scalar, memory) = prepared.into_parts();
    Ok(GeneratedExpression {
        scalar,
        _preparation_memory: memory,
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

pub(crate) fn evaluate_generated_expression_with_control(
    expression: &crate::ScalarExpr,
    row: &dyn RowLookup,
    control: &ProductionControl<'_>,
) -> Result<Produced<uqa_core::Value>, SQLError> {
    crate::eval_generated_scalar_with_control(expression, row, control)
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
