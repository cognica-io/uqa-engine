//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Output eligibility borrows labels and admits catalog, retained-value and type scratch.

use super::{OutputPruningContext, QueryBlockPlan, QueryPlan, SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::ast::FunctionVolatility;

pub(super) fn needed_by_grouping(
    block: &QueryBlockPlan,
    index: usize,
    source: Option<&uqa_sql::plan::AnalyzedSource>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::with_projection_label(&block.projections[index], control, |name| {
        for expression in block
            .group_by
            .iter()
            .chain(block.grouping_sets.iter().flatten())
        {
            control.check()?;
            let required = match expression {
                ScalarExpr::Column(column) if column.as_str() == name => {
                    source.is_none_or(|source| {
                        uqa_sql::semantics::grouping_sets::grouping_output_name(
                            expression,
                            &source.schema,
                            None,
                        )
                        .is_some()
                    })
                }
                ScalarExpr::Position(position) => *position == index,
                _ => false,
            };
            if required {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

pub(super) fn needed_by_ordering(
    block: &QueryBlockPlan,
    index: usize,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let projection = &block.projections[index];
    uqa_sql::semantics::with_projection_label(projection, control, |name| {
        for order in &block.order_by {
            control.check()?;
            if matches!(&order.expr, ScalarExpr::Column(column) if column.as_str() == name)
                || matches!(order.expr, ScalarExpr::Position(position) if position == index)
                || matches!(order.expr, ScalarExpr::Literal(Value::Int(position)) if uqa_sql::semantics::query_output_position(position, block.projections.len(), "ORDER BY").is_ok_and(|position| position == index))
                || uqa_sql::semantics::aggregates::exprs_match_with_control(
                    &order.expr,
                    &projection.expr,
                    control,
                )?
            {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

pub(super) fn removable(
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    context: OutputPruningContext<'_>,
    visible_ctes: &dyn Fn(&str) -> bool,
) -> Result<bool, SQLError> {
    let control = &context.production;
    let mut removable = true;
    let mut contains_subquery = false;
    expression.try_visit(&mut |part| {
        control.check()?;
        match part {
            ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } if !uqa_sql::semantics::aggregates::is_builtin_aggregate(part) => {
                let name =
                    uqa_sql::semantics::builtin_function_dispatch_name_with_control(name, control)?;
                if context
                    .aggregates
                    .is_registered_aggregate_with_control(&name, control)?
                    || uqa_sql::semantics::sets::validation::builtin_returns_set(&name)
                    || binding.as_ref().is_none_or(|binding| !binding.builtin)
                    || uqa_sql::semantics::volatility::builtin_function_volatility_with_control(
                        &name,
                        binding.as_ref(),
                        args.len(),
                        control,
                    )? == FunctionVolatility::Volatile
                {
                    removable = false;
                }
            }
            ScalarExpr::Cast { ty, .. } => {
                let immutable = match uqa_sql::ColumnType::from_sql_name_with_control(ty, control) {
                    Ok(ty) => uqa_sql::type_resolution::immutable_cast_type(&ty),
                    Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                        return Err(error)
                    }
                    Err(_) => false,
                };
                removable &= immutable;
            }
            ScalarExpr::WindowCall { .. } | ScalarExpr::CompositeRow { .. } => removable = false,
            ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. } => contains_subquery = true,
            _ => {}
        }
        Ok(true)
    })?;
    if !removable || !contains_subquery {
        return Ok(removable);
    }
    match context.volatility {
        Some(catalog) => Ok(!uqa_sql::semantics::volatility::scalar_in_scope_contains_volatile_function_with_control(catalog, expression, subqueries, visible_ctes, control)?),
        None => Ok(false),
    }
}
