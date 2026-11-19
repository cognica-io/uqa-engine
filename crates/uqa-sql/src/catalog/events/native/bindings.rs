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
    crate::catalog::work::inline(validate_condition_with_work(
        rule,
        row,
        context,
        control,
        &mut (),
    ))
}

pub async fn validate_condition_with_work(
    rule: &StoredRule,
    row: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    control.check()?;
    let Some((plan, binding)) = super::condition_parts(rule)? else {
        return Ok(());
    };
    binding
        .validate_captured_with_work(rule.definition.event, plan, control, work, &mut |name| {
            if row.has_unqualified_column(name) {
                Ok(())
            } else {
                Err(super::invalid("missing captured condition column"))
            }
        })
        .await?;
    let schema = binding.captured_row_schema(row, control)?;
    references(&plan.scalar, context, control, work).await?;
    for query in &plan.subqueries {
        let query = crate::plan::PlanRef::Query(query);
        work.checkpoint().await?;
        context.references.plan_with_work(query, work).await?;
        let mut observer = ScalarTypes { context, control };
        crate::plan::visit_scalar_nodes_with_work(query, control, work, &mut observer).await?;
    }
    crate::ir::observation::visit_in(
        &plan.scalar,
        control,
        work,
        crate::ir::children::Scope::Complete,
        &mut |node| {
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
            Ok(crate::ir::observation::Decision::Descend)
        },
    )
    .await?;
    if crate::semantics::sets::validation::expression_may_return_set_with_work(
        context.routines,
        context.routines,
        &plan.scalar,
        &schema,
        &[],
        control,
        work,
    )
    .await?
    {
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

struct ScalarTypes<'a, 'c, 's> {
    context: &'a NativeScalarContext<'c, 's>,
    control: &'a ProductionControl<'a>,
}

impl crate::ir::observation::ScalarObserver for ScalarTypes<'_, '_, '_> {
    async fn scalar<W: crate::catalog::CatalogWork + Send>(
        &mut self,
        node: &ScalarExpr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        match node {
            ScalarExpr::Cast { ty, .. } | ScalarExpr::TypedLiteral { ty, .. } => {
                self.context.types.stored_name_with_work(ty, work).await?;
            }
            ScalarExpr::CompositeRow { binding, .. } => {
                self.context
                    .types
                    .stored_name_with_work(&binding.ty, work)
                    .await?;
            }
            ScalarExpr::RetainedLiteral(literal) => {
                if let Some(ty) = &literal.ty {
                    self.context.types.column_type_with_work(ty, work).await?;
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
            } => self.context.types.column_type_with_work(ty, work).await,
            _ => Ok(()),
        }
    }
}

async fn references(
    scalar: &ScalarExpr,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    context.references.scalar_with_work(scalar, work).await?;
    crate::ir::observation::visit_all(scalar, control, work, &mut ScalarTypes { context, control })
        .await
}

#[cfg(test)]
mod tests;
