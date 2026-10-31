//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call and retained-result rewrites keep their recursive storage separate from unrelated scalar variants.

use super::{
    constants, finish_scalar, optimize_boolean, optimize_coalesce, optimize_list,
    optimize_optional, optimize_scalar, optimize_scalar_slot, OptimizerConfig, SQLError,
    ScalarExpr,
};

pub(super) fn optimize_collated(
    expression: ScalarExpr,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let ScalarExpr::Collated {
        expr,
        ty,
        collation,
    } = expression
    else {
        unreachable!("retained collation expression");
    };
    let selected_arm = matches!(*expr, ScalarExpr::Case { .. });
    let mut expr = super::rewrite_box(expr, |expression| match expression {
        expression @ ScalarExpr::Binary { .. } => {
            super::optimize_noncall_scalar(expression, config, Some(&ty))
        }
        expression => optimize_scalar(expression, config),
    })?;
    if let Some(oid) = collation.implicit_oid() {
        match expr.as_mut() {
            ScalarExpr::TypedLiteral {
                ty: name,
                bound_type,
                collation_oid,
                ..
            } => {
                // Simplification may select an arm whose modifier is more specific than the original CASE result.
                let ty = bound_type
                    .take()
                    .filter(|inner| {
                        selected_arm
                            && inner.without_type_modifiers() == ty.without_type_modifiers()
                    })
                    .unwrap_or(ty);
                *name = ty.catalog_name();
                *bound_type = Some(ty);
                *collation_oid = Some(oid);
                return Ok(*expr);
            }
            ScalarExpr::Literal(_) => {
                let ScalarExpr::Literal(value) = *expr else {
                    unreachable!()
                };
                return Ok(ScalarExpr::TypedLiteral {
                    value,
                    ty: ty.catalog_name(),
                    bound_type: Some(ty),
                    collation_oid: Some(oid),
                    composite_source: None,
                    parameter_index: None,
                });
            }
            _ => {}
        }
    }
    Ok(ScalarExpr::Collated {
        expr,
        ty,
        collation,
    })
}

pub(super) fn optimize_call(
    expression: ScalarExpr,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let ScalarExpr::Func {
        order_syntax,
        name,
        mut binding,
        mut args,
        distinct,
        mut order_by,
        filter,
    } = expression
    else {
        unreachable!("function expression");
    };
    if !distinct && order_by.is_empty() && filter.is_none() {
        if let (Some(context), Some(selected)) =
            (config.routine_inlining.as_ref(), binding.as_ref())
        {
            if let Some((selected, materialized)) = context.materialize_call(selected, &args)? {
                binding = Some(selected);
                args = materialized;
            }
        }
    }
    for order in &mut order_by {
        optimize_scalar_slot(&mut order.expr, config)?;
    }
    let row_distinct = binding.as_ref().is_some_and(|binding| {
        binding.dispatch == Some(uqa_sql::ast::FunctionDispatch::RowDistinct)
    });
    let coalesce = !row_distinct && constants::is_coalesce(&name, binding.as_ref());
    let args = if row_distinct {
        // PostgreSQL simplifies the generated disjunction in order, including an unreachable set-returning field after a constant true comparison.
        match optimize_boolean(args, false, config)? {
            ScalarExpr::Or(items) => items,
            simplified => return Ok(simplified),
        }
    } else if coalesce {
        args
    } else {
        optimize_list(args, config)?
    };
    let mut optimized = ScalarExpr::Func {
        order_syntax,
        name,
        binding,
        args,
        distinct,
        order_by,
        filter,
    };
    if coalesce {
        optimized = optimize_coalesce(optimized, config)?;
    }
    let ScalarExpr::Func { filter, .. } = &mut optimized else {
        unreachable!("function argument optimization preserves its call");
    };
    *filter = optimize_optional(filter.take(), config)?;
    finish_scalar(optimized, false, false, config, None)
}
