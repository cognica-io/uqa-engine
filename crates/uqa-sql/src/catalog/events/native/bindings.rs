//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type captured rule conditions in their original row namespace without changing stored plans.

use crate::{
    catalog::{events::StoredRule, scalar_bindings::NativeScalarContext},
    RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub fn validate_condition(
    rule: &StoredRule,
    row: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let Some((plan, binding)) = super::condition_parts(rule)? else {
        return Ok(());
    };
    binding.validate_captured(rule.definition.event, plan, control, &mut |name| {
        if row.has_unqualified_column(name) {
            Ok(())
        } else {
            Err(super::invalid("missing captured condition column"))
        }
    })?;
    let schema = binding.captured_row_schema(row, control)?;
    references(&plan.scalar, context, control)?;
    for query in &plan.subqueries {
        let query = crate::plan::PlanRef::Query(query);
        context.references.plan(query)?;
        query.try_visit_scalar_expressions(&mut |scalar| references(scalar, context, control))?;
    }
    plan.scalar.try_visit(&mut |node| {
        control.check()?;
        let invalid = match node {
            ScalarExpr::WindowCall { .. } => true,
            ScalarExpr::Func { name, binding, .. } => {
                crate::semantics::is_builtin_aggregate_call(name, binding.as_ref())
                    || context
                        .routines
                        .has_registered_aggregate_function_with_control(name, control)?
            }
            _ => false,
        };
        if invalid {
            return Err(super::invalid("aggregate or window in a rule condition"));
        }
        Ok(true)
    })?;
    if crate::semantics::sets::validation::expression_may_return_set_with_control(
        context.routines,
        context.routines,
        &plan.scalar,
        &schema,
        &[],
        control,
    )? {
        return Err(super::invalid("set-valued rule condition"));
    }
    let whole_rows =
        if crate::semantics::rules::action_binding::rule_condition_plan_references_whole_row(plan) {
            [
                binding.current_whole_row("old", &rule.definition.table, row, control)?,
                binding.current_whole_row("new", &rule.definition.table, row, control)?,
            ]
        } else {
            [None, None]
        };
    crate::binding::validate_stored_rule_condition_with_control(
        context.routines,
        plan,
        &schema,
        context.binding.copy_with_control(control)?,
        whole_rows,
        control,
    )
}

fn references(
    scalar: &ScalarExpr,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    context.references.scalar(scalar)?;
    scalar.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } => {
                context.types.stored_name(ty)?;
            }
            ScalarExpr::CompositeRow { binding, .. } => context.types.stored_name(&binding.ty)?,
            ScalarExpr::RetainedLiteral(literal) => {
                if let Some(ty) = &literal.ty {
                    context.types.column_type(ty)?;
                }
            }
            _ => (),
        }
        match node {
            ScalarExpr::Collated { ty, .. }
            | ScalarExpr::TypedLiteral {
                bound_type: Some(ty),
                ..
            }
            | ScalarExpr::CompositeRow {
                bound_type: Some(ty),
                ..
            } => context.types.column_type(ty)?,
            _ => (),
        }
        Ok::<_, SQLError>(true)
    })
}

#[cfg(test)]
mod tests;
