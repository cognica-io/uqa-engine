//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read stored index expressions against their original row and routine declarations.

use crate::{
    ast::{ColumnDef, Expr, IndexKey},
    binding::production::ControlledBindingContext,
    catalog::index::IndexDefinition,
    plan::ExpressionPlan,
    routines::{native::NativeRoutineReferences, RoutineResolution},
    ColumnType, RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

pub struct IndexBindingContext<'a, 'b> {
    pub routines: &'a dyn RoutineResolution,
    pub references: &'a NativeRoutineReferences<'b>,
    pub types: &'a crate::catalog::type_references::TypeReferences<'b>,
    pub binding: &'a ControlledBindingContext<'b>,
}

/// Validate the original key result types and predicate without rebuilding an index, changing a binding, evaluating a scalar or compiling a routine body. Empty legacy type vectors remain absent; document fields without declarations retain their open row scope.
pub fn validate_bindings(
    keys: &[IndexKey],
    definition: &IndexDefinition,
    columns: Option<&[ColumnDef]>,
    context: &IndexBindingContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if !definition.key_types.is_empty() && definition.key_types.len() != keys.len() {
        return Err(invalid("key type count"));
    }
    for ty in &definition.key_types {
        context.types.column_type(ty)?;
    }
    let mut schema = RowSchema::with_column_metadata_with_control(
        None,
        columns
            .into_iter()
            .flatten()
            .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
        control,
    )?;
    if columns.is_none() {
        schema = RowSchema::with_open_columns_with_control(&schema, None, control)?;
    }
    for (position, key) in keys.iter().enumerate() {
        control.check()?;
        let expected = definition.key_types.get(position);
        match key {
            IndexKey::Column(name) => {
                if let Some(column) = columns
                    .into_iter()
                    .flatten()
                    .find(|column| column.name == *name)
                {
                    if expected.is_some_and(|expected| *expected != column.ty) {
                        return Err(invalid("column key type"));
                    }
                }
            }
            IndexKey::Expression(expression) => {
                let ty = expression_type(expression, &schema, context, control)?;
                // Ordinary index declaration resolves an unknown result to text.
                if expected.is_some_and(|expected| {
                    *expected != *ty.as_deref().unwrap_or(&ColumnType::Text)
                }) {
                    return Err(invalid("expression key type"));
                }
            }
        }
    }
    if let Some(predicate) = &definition.predicate {
        let ty = expression_type(predicate, &schema, context, control)?;
        if !matches!(ty.as_deref(), Some(ColumnType::Boolean) | None) {
            return Err(invalid("non-boolean predicate"));
        }
        if ty.is_none() {
            if let Expr::Literal(value) = predicate.as_ref() {
                crate::expr::cast_value(value, "boolean")?;
            }
        }
    }
    control.check()?;
    Ok(())
}

fn expression_type(
    expression: &Expr,
    schema: &RowSchema,
    context: &IndexBindingContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    crate::schema::dependencies::walk_schema_expr_with_control(expression, control, &mut |node| {
        if matches!(
            node,
            Expr::Param(_) | Expr::Default | Expr::InternalColumn(_) | Expr::WindowCall { .. }
        ) {
            return Err(invalid("non-scalar index expression"));
        }
        match node {
            Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. } => {
                context.types.stored_name(ty)?;
            }
            Expr::CompositeRow { binding, .. } => context.types.stored_name(&binding.ty)?,
            Expr::RetainedLiteral(literal) => {
                if let Some(ty) = &literal.0.ty {
                    context.types.column_type(ty)?;
                }
            }
            _ => (),
        }
        Ok(())
    })?;
    context.references.expression(expression)?;
    let plan = ExpressionPlan::lower_borrowed_for_rewrite(expression, control)?;
    if !plan.queries().is_empty() {
        return Err(invalid("query-valued index expression"));
    }
    plan.scalar().try_visit(&mut |node| {
        control.check()?;
        if let ScalarExpr::Func { name, binding, .. } = node {
            if crate::semantics::is_builtin_aggregate_call(name, binding.as_ref())
                || context
                    .routines
                    .has_registered_aggregate_function_with_control(name, control)?
            {
                return Err(invalid("aggregate index expression"));
            }
        }
        Ok(true)
    })?;
    if crate::semantics::sets::validation::expression_may_return_set_with_control(
        context.routines,
        context.routines,
        plan.scalar(),
        schema,
        &[],
        control,
    )? {
        return Err(invalid("set-valued index expression"));
    }
    crate::binding::analyze_stored_scalar_with_control(
        context.routines,
        plan.scalar(),
        schema,
        context.binding.copy_with_control(control)?,
        control,
    )
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native index binding: {detail}"))
}

#[cfg(test)]
mod tests;
