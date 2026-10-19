//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar simplification with SQL errors preserved at the planning boundary.

use super::{
    AssignmentPlan, BTreeMap, OptimizerConfig, ProjectionPlan, ScalarExpr, ScalarFrameBound, Value,
};
use uqa_sql::SQLError;

mod conditional;
pub(super) mod constants;
mod nodes;

use conditional::{optimize_boolean, optimize_case, optimize_coalesce};
use constants::fold_authorized_literal;

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
        optimized = merge_vector_thresholds(optimized);
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
        Some(expression) => Ok(Some(Box::new(optimize_scalar(*expression, config)?))),
        None => Ok(None),
    }
}

fn optimize_scalar(
    expression: ScalarExpr,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    match expression {
        expression @ ScalarExpr::Collated { .. } => nodes::optimize_collated(expression, config),
        expression @ ScalarExpr::Func { .. } => nodes::optimize_call(expression, config),
        other => optimize_noncall_scalar(other, config),
    }
}

fn optimize_noncall_scalar(
    expression: ScalarExpr,
    config: &OptimizerConfig,
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
                lhs: Box::new(operand(*lhs)?),
                rhs: Box::new(operand(*rhs)?),
            }
        }
        ScalarExpr::UnaryMinus(inner) => {
            ScalarExpr::UnaryMinus(Box::new(optimize_scalar(*inner, config)?))
        }
        ScalarExpr::Not(inner) => {
            let inner = optimize_scalar(*inner, config)?;
            match inner {
                ScalarExpr::Not(inner) if config.enable_boolean_simplify => *inner,
                other => ScalarExpr::Not(Box::new(other)),
            }
        }
        ScalarExpr::And(items) => optimize_boolean(items, true, config)?,
        ScalarExpr::Or(items) => optimize_boolean(items, false, config)?,
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(optimize_scalar(*expr, config)?),
            negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(optimize_scalar(*expr, config)?),
            low: Box::new(optimize_scalar(*low, config)?),
            high: Box::new(optimize_scalar(*high, config)?),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(optimize_scalar(*expr, config)?),
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
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => optimize_case(base, when, else_branch, config)?,
        ScalarExpr::Cast { expr, ty, implicit } => ScalarExpr::Cast {
            implicit,
            expr: Box::new(optimize_scalar(*expr, config)?),
            ty,
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(optimize_scalar(*expr, config)?),
            subquery,
            negated,
        },
        other => other,
    };
    finish_scalar(optimized, integer_syntax, row_syntax, config)
}

fn finish_scalar(
    optimized: ScalarExpr,
    integer_syntax: bool,
    row_syntax: bool,
    config: &OptimizerConfig,
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
    )?;
    let optimized = constants::fold_composite_constructor(
        constants::retain_record_result(optimized, row_syntax),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
    )?;
    let optimized = fold_authorized_literal(
        optimized,
        config.constant_evaluator,
        config.builtin_permissions.as_deref(),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.routines as &dyn uqa_sql::FunctionTypeResolver),
    )?;
    Ok(if integer_syntax {
        optimized
    } else {
        constants::retain_computed_integer(optimized, None)
    })
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

fn merge_vector_thresholds(expression: ScalarExpr) -> ScalarExpr {
    match expression {
        ScalarExpr::And(items) => {
            let mut by_field: BTreeMap<String, (ScalarExpr, f64)> = BTreeMap::new();
            let mut others = Vec::new();
            for item in items {
                if let ScalarExpr::Func { name, args, .. } = &item {
                    if name.eq_ignore_ascii_case("knn_match") && args.len() >= 3 {
                        if let (
                            ScalarExpr::Literal(Value::Str(field)),
                            ScalarExpr::Literal(Value::Float(threshold)),
                        ) = (&args[0], &args[2])
                        {
                            let entry = by_field
                                .entry(field.clone())
                                .or_insert_with(|| (item.clone(), *threshold));
                            if *threshold > entry.1 {
                                entry.1 = *threshold;
                                if let ScalarExpr::Func { args, .. } = &mut entry.0 {
                                    args[2] = ScalarExpr::Literal(Value::Float(*threshold));
                                }
                            }
                            continue;
                        }
                    }
                }
                others.push(merge_vector_thresholds(item));
            }
            others.extend(by_field.into_values().map(|(expression, _)| expression));
            if others.is_empty() {
                ScalarExpr::Literal(Value::Bool(true))
            } else if others.len() == 1 {
                others.remove(0)
            } else {
                ScalarExpr::And(others)
            }
        }
        ScalarExpr::Or(items) => {
            ScalarExpr::Or(items.into_iter().map(merge_vector_thresholds).collect())
        }
        ScalarExpr::Not(inner) => ScalarExpr::Not(Box::new(merge_vector_thresholds(*inner))),
        other => other,
    }
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
                &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
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
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
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
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
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
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"));
    }
}
