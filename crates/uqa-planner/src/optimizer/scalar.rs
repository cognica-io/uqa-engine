//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar simplification with SQL errors preserved at the planning boundary.

use super::{AssignmentPlan, OptimizerConfig, ProjectionPlan, ScalarExpr, ScalarFrameBound, Value};
use uqa_sql::SQLError;

mod conditional;
pub(super) mod constants;
mod nodes;
pub(super) mod owned;
pub(super) mod qualification;
mod vector_thresholds;
pub(crate) use qualification::optimize_qualification;
pub(crate) use qualification::optimize_qualification_owned;

use conditional::{optimize_boolean, optimize_case, optimize_coalesce};
use constants::fold_selected_literal;
use vector_thresholds::merge_vector_thresholds;

#[cfg(test)]
mod storage_tests;

pub(super) fn optimize_assignments(
    assignments: &mut [AssignmentPlan],
    config: &OptimizerConfig,
) -> Result<(), SQLError> {
    for assignment in assignments {
        optimize_scalar_slot(&mut assignment.value, config)?;
    }
    Ok(())
}

pub(super) fn optimize_projections(
    projections: &mut [ProjectionPlan],
    config: &OptimizerConfig,
) -> Result<(), SQLError> {
    for projection in projections {
        if projection.alias.is_none()
            && !matches!(
                projection.expr,
                ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
            )
        {
            projection.alias = Some(uqa_sql::semantics::projection_label_at(projection));
        }
        optimize_scalar_slot(&mut projection.expr, config)?;
    }
    Ok(())
}

pub(super) fn optimize_scalar_slot(
    expression: &mut ScalarExpr,
    config: &OptimizerConfig,
) -> Result<(), SQLError> {
    let placeholder = ScalarExpr::Literal(Value::Null);
    let mut optimized = optimize_scalar(std::mem::replace(expression, placeholder), config)?;
    if config.enable_vector_threshold_merge {
        optimized = merge_vector_thresholds(optimized, &config.production)?;
    }
    *expression = optimized;
    Ok(())
}

fn optimize_list(
    items: Vec<ScalarExpr>,
    config: &OptimizerConfig,
) -> Result<Vec<ScalarExpr>, SQLError> {
    let mut items = items;
    for item in &mut items {
        let original = std::mem::replace(item, ScalarExpr::Literal(Value::Null));
        *item = optimize_scalar(original, config)?;
    }
    Ok(items)
}

fn optimize_optional(
    expression: Option<Box<ScalarExpr>>,
    config: &OptimizerConfig,
) -> Result<Option<Box<ScalarExpr>>, SQLError> {
    match expression {
        Some(expression) => Ok(Some(optimize_box(expression, config)?)),
        None => Ok(None),
    }
}

fn rewrite_box(
    mut expression: Box<ScalarExpr>,
    rewrite: impl FnOnce(ScalarExpr) -> Result<ScalarExpr, SQLError>,
) -> Result<Box<ScalarExpr>, SQLError> {
    let original = std::mem::replace(expression.as_mut(), ScalarExpr::Literal(Value::Null));
    *expression = rewrite(original)?;
    Ok(expression)
}

fn optimize_box(
    expression: Box<ScalarExpr>,
    config: &OptimizerConfig,
) -> Result<Box<ScalarExpr>, SQLError> {
    rewrite_box(expression, |expression| optimize_scalar(expression, config))
}

fn optimize_scalar(
    expression: ScalarExpr,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    match expression {
        expression @ ScalarExpr::Collated { .. } => nodes::optimize_collated(expression, config),
        expression @ ScalarExpr::Func { .. } => nodes::optimize_call(expression, config),
        other => optimize_noncall_scalar(other, config, None),
    }
}

fn optimize_noncall_scalar(
    expression: ScalarExpr,
    config: &OptimizerConfig,
    result_type: Option<&uqa_sql::ColumnType>,
) -> Result<ScalarExpr, SQLError> {
    let integer_syntax = matches!(&expression, ScalarExpr::Literal(Value::Int(_)));
    let row_syntax = matches!(&expression, ScalarExpr::Row(_));
    let optimized = match expression {
        ScalarExpr::Array(items) => ScalarExpr::Array(optimize_list(items, config)?),
        ScalarExpr::Row(items) => ScalarExpr::Row(optimize_list(items, config)?),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let row_comparison = matches!((&*lhs, &*rhs), (ScalarExpr::Row(_), ScalarExpr::Row(_)));
            let operand = |expression| match (row_comparison, expression) {
                // Preserve the syntax selecting row comparison while folding its fields. The enclosing constant comparison can still fold as a whole; record constants in projections and grouping remain foldable.
                (true, ScalarExpr::Row(items)) => {
                    Ok(ScalarExpr::Row(optimize_list(items, config)?))
                }
                (_, expression) => optimize_scalar(expression, config),
            };
            ScalarExpr::Binary {
                op,
                lhs: rewrite_box(lhs, operand)?,
                rhs: rewrite_box(rhs, operand)?,
            }
        }
        ScalarExpr::UnaryMinus(inner) => ScalarExpr::UnaryMinus(optimize_box(inner, config)?),
        ScalarExpr::Not(inner) => {
            let inner = optimize_box(inner, config)?;
            if config.enable_boolean_simplify && matches!(*inner, ScalarExpr::Not(_)) {
                let ScalarExpr::Not(inner) = *inner else {
                    unreachable!()
                };
                *inner
            } else {
                ScalarExpr::Not(inner)
            }
        }
        ScalarExpr::And(items) => optimize_boolean(items, true, config)?,
        ScalarExpr::Or(items) => optimize_boolean(items, false, config)?,
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: optimize_box(expr, config)?,
            negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: optimize_box(expr, config)?,
            low: optimize_box(low, config)?,
            high: optimize_box(high, config)?,
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: optimize_box(expr, config)?,
            list: optimize_list(list, config)?,
            negated,
        },
        ScalarExpr::WindowCall {
            name,
            args,
            mut spec,
            filter,
            modifiers,
        } => {
            spec.partition_by = optimize_list(spec.partition_by, config)?;
            for order in &mut spec.order_by {
                optimize_scalar_slot(&mut order.expr, config)?;
            }
            if let Some(frame) = &mut spec.frame {
                optimize_frame_bound(&mut frame.start, config)?;
                optimize_frame_bound(&mut frame.end, config)?;
            }
            ScalarExpr::WindowCall {
                name,
                args: optimize_list(args, config)?,
                spec,
                filter: optimize_optional(filter, config)?,
                modifiers,
            }
        }
        expression @ ScalarExpr::Case { .. } => optimize_case(expression, config)?,
        ScalarExpr::Cast { expr, ty, implicit } => ScalarExpr::Cast {
            implicit,
            expr: optimize_box(expr, config)?,
            ty,
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: optimize_box(expr, config)?,
            subquery,
            negated,
        },
        other => other,
    };
    finish_scalar(optimized, integer_syntax, row_syntax, config, result_type)
}

fn finish_scalar(
    optimized: ScalarExpr,
    integer_syntax: bool,
    row_syntax: bool,
    config: &OptimizerConfig,
    result_type: Option<&uqa_sql::ColumnType>,
) -> Result<ScalarExpr, SQLError> {
    if let (
        Some(context),
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            distinct: false,
            order_by,
            filter: None,
            ..
        },
    ) = (config.routine_inlining.as_ref(), &optimized)
    {
        if order_by.is_empty() {
            if let Some(inlined) = context.prepare(binding, args, &config.active_inline_routines)? {
                let mut nested = config.clone();
                nested.active_inline_routines.push(inlined.identity);
                return optimize_scalar(inlined.expression, &nested)
                    .map_err(|error| match inlined.error_context {
                        Some(context) => error.with_context(context),
                        None => error,
                    })
                    .map(|expression| constants::retain_record_result(expression, false))
                    .map(|expression| constants::retain_computed_integer(expression, None));
            }
        }
    }
    constants::check_inlined_similar_substring(
        &optimized,
        config.builtin_permissions.as_deref(),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.routines as &dyn uqa_sql::FunctionTypeResolver),
        &config.production,
    )?;
    let optimized = constants::fold_composite_constructor(
        constants::retain_record_result(optimized, row_syntax),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
        &config.production,
    )?;
    let optimized = fold_selected_literal(
        optimized,
        |expression, control| (config.constant_evaluator)(expression, &[], control),
        config.builtin_permissions.as_deref(),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.routines as &dyn uqa_sql::FunctionTypeResolver),
        result_type,
        config,
    )?;
    let optimized = if integer_syntax {
        optimized
    } else {
        constants::retain_computed_integer(optimized, None)
    };
    if config.retain_values_constants {
        constants::retain_large_literal(optimized, config.constant_storage.as_deref())
    } else {
        Ok(optimized)
    }
}

fn optimize_frame_bound(
    bound: &mut ScalarFrameBound,
    config: &OptimizerConfig,
) -> Result<(), SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            optimize_scalar_slot(expression, config)?;
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_sql::ast::BinaryOp;

    #[test]
    fn scalar_optimization_retains_array_coercion_origin() {
        for implicit in [false, true] {
            let expected = ScalarExpr::Cast {
                implicit,
                expr: Box::new(ScalarExpr::Array(vec![ScalarExpr::Column("value".into())])),
                ty: "bigint[]".into(),
            };
            let mut expression = expected.clone();
            optimize_scalar_slot(
                &mut expression,
                &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
            )
            .unwrap();
            assert_eq!(expression, expected);
        }
    }

    #[test]
    fn folds_literal_date_value_without_erasing_its_declared_type() {
        let mut expression = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Literal(Value::Str("1993-07-01".into()))),
            ty: "date".into(),
        };

        optimize_scalar_slot(
            &mut expression,
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap();

        assert!(matches!(
            expression,
            ScalarExpr::Cast { ty, .. } if ty == "date"
        ));
    }

    #[test]
    fn folds_nested_literal_arithmetic_bottom_up() {
        let mut expression = ScalarExpr::Binary {
            op: BinaryOp::Multiply,
            lhs: Box::new(ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(2))),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(3))),
            }),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(4))),
        };

        optimize_scalar_slot(
            &mut expression,
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap();

        assert!(matches!(
            expression,
            ScalarExpr::TypedLiteral {
                value: Value::Int(20),
                bound_type: Some(uqa_sql::ColumnType::Integer),
                ..
            }
        ));
    }

    #[test]
    fn reports_invalid_constant_input_during_planning() {
        let mut expression = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Literal(Value::Str("not-an-integer".into()))),
            ty: "integer".into(),
        };
        let error = optimize_scalar_slot(
            &mut expression,
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"));
    }
}
