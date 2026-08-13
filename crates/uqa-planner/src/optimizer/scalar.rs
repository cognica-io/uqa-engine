//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar Boolean simplification and vector-threshold rewrites.

use super::{
    AssignmentPlan, BTreeMap, OptimizerConfig, ProjectionPlan, ScalarExpr, ScalarFrameBound, Value,
};
use uqa_sql::expr::{cast_value, eval_binary_values, truthy};

pub(super) fn optimize_assignments(assignments: &mut [AssignmentPlan], config: &OptimizerConfig) {
    for assignment in assignments {
        optimize_scalar_slot(&mut assignment.value, config);
    }
}

pub(super) fn optimize_projections(projections: &mut [ProjectionPlan], config: &OptimizerConfig) {
    for projection in projections {
        optimize_scalar_slot(&mut projection.expr, config);
    }
}

pub(super) fn optimize_scalar_slot(expression: &mut ScalarExpr, config: &OptimizerConfig) {
    let placeholder = ScalarExpr::Literal(Value::Null);
    let mut optimized = optimize_scalar(std::mem::replace(expression, placeholder), config);
    if config.enable_vector_threshold_merge {
        optimized = merge_vector_thresholds(optimized);
    }
    *expression = optimized;
}

fn optimize_scalar(expression: ScalarExpr, config: &OptimizerConfig) -> ScalarExpr {
    let optimized = match expression {
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op,
            lhs: Box::new(optimize_scalar(*lhs, config)),
            rhs: Box::new(optimize_scalar(*rhs, config)),
        },
        ScalarExpr::Not(inner) => {
            let inner = optimize_scalar(*inner, config);
            if config.enable_boolean_simplify {
                match inner {
                    ScalarExpr::Literal(Value::Bool(value)) => {
                        ScalarExpr::Literal(Value::Bool(!value))
                    }
                    ScalarExpr::Not(inner) => *inner,
                    other => ScalarExpr::Not(Box::new(other)),
                }
            } else {
                ScalarExpr::Not(Box::new(inner))
            }
        }
        ScalarExpr::And(items) => {
            let items = items
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect();
            if config.enable_boolean_simplify {
                simplify_and(items)
            } else {
                ScalarExpr::And(items)
            }
        }
        ScalarExpr::Or(items) => {
            let items = items
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect();
            if config.enable_boolean_simplify {
                simplify_or(items)
            } else {
                ScalarExpr::Or(items)
            }
        }
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(optimize_scalar(*expr, config)),
            negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(optimize_scalar(*expr, config)),
            low: Box::new(optimize_scalar(*low, config)),
            high: Box::new(optimize_scalar(*high, config)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(optimize_scalar(*expr, config)),
            list: list
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect(),
            negated,
        },
        ScalarExpr::Func {
            name,
            args,
            distinct,
            mut order_by,
            filter,
        } => {
            for order in &mut order_by {
                order.expr = optimize_scalar(
                    std::mem::replace(&mut order.expr, ScalarExpr::Literal(Value::Null)),
                    config,
                );
            }
            ScalarExpr::Func {
                name,
                args: args
                    .into_iter()
                    .map(|argument| optimize_scalar(argument, config))
                    .collect(),
                distinct,
                order_by,
                filter: filter.map(|filter| Box::new(optimize_scalar(*filter, config))),
            }
        }
        ScalarExpr::WindowCall {
            name,
            args,
            mut spec,
        } => {
            spec.partition_by = spec
                .partition_by
                .into_iter()
                .map(|expression| optimize_scalar(expression, config))
                .collect();
            for order in &mut spec.order_by {
                order.expr = optimize_scalar(
                    std::mem::replace(&mut order.expr, ScalarExpr::Literal(Value::Null)),
                    config,
                );
            }
            if let Some(frame) = &mut spec.frame {
                optimize_frame_bound(&mut frame.start, config);
                optimize_frame_bound(&mut frame.end, config);
            }
            ScalarExpr::WindowCall {
                name,
                args: args
                    .into_iter()
                    .map(|argument| optimize_scalar(argument, config))
                    .collect(),
                spec,
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base.map(|base| Box::new(optimize_scalar(*base, config))),
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    (
                        optimize_scalar(condition, config),
                        optimize_scalar(result, config),
                    )
                })
                .collect(),
            else_branch: else_branch.map(|branch| Box::new(optimize_scalar(*branch, config))),
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(optimize_scalar(*expr, config)),
            ty,
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(optimize_scalar(*expr, config)),
            subquery,
            negated,
        },
        other => other,
    };
    fold_literal_expression(optimized)
}

/// Fold deterministic scalar work whose complete input is already in the
/// physical plan. Failed folds stay in the plan so SQL errors retain their
/// normal execution-time behavior.
fn fold_literal_expression(expression: ScalarExpr) -> ScalarExpr {
    match expression {
        ScalarExpr::Cast { expr, ty } => match *expr {
            ScalarExpr::Literal(value) if is_integer_type(&ty) => ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(value)),
                ty,
            },
            ScalarExpr::Literal(value) => cast_value(&value, &ty)
                .map(ScalarExpr::Literal)
                .unwrap_or_else(|_| ScalarExpr::Cast {
                    expr: Box::new(ScalarExpr::Literal(value)),
                    ty,
                }),
            expr => ScalarExpr::Cast {
                expr: Box::new(expr),
                ty,
            },
        },
        ScalarExpr::Binary { op, lhs, rhs } => match (*lhs, *rhs) {
            (ScalarExpr::Literal(lhs), ScalarExpr::Literal(rhs)) => {
                eval_binary_values(op, &lhs, &rhs)
                    .map(ScalarExpr::Literal)
                    .unwrap_or_else(|_| ScalarExpr::Binary {
                        op,
                        lhs: Box::new(ScalarExpr::Literal(lhs)),
                        rhs: Box::new(ScalarExpr::Literal(rhs)),
                    })
            }
            (lhs, rhs) => ScalarExpr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        },
        ScalarExpr::Not(inner) => match *inner {
            ScalarExpr::Literal(Value::Null) => ScalarExpr::Literal(Value::Null),
            ScalarExpr::Literal(value) => ScalarExpr::Literal(Value::Bool(!truthy(&value))),
            inner => ScalarExpr::Not(Box::new(inner)),
        },
        ScalarExpr::IsNull { expr, negated } => match *expr {
            ScalarExpr::Literal(value) => {
                let is_null = matches!(value, Value::Null);
                ScalarExpr::Literal(Value::Bool(if negated { !is_null } else { is_null }))
            }
            expr => ScalarExpr::IsNull {
                expr: Box::new(expr),
                negated,
            },
        },
        ScalarExpr::Array(items)
            if items
                .iter()
                .all(|item| matches!(item, ScalarExpr::Literal(_))) =>
        {
            ScalarExpr::Literal(Value::List(
                items
                    .into_iter()
                    .filter_map(|item| match item {
                        ScalarExpr::Literal(value) => Some(value),
                        _ => None,
                    })
                    .collect(),
            ))
        }
        other => other,
    }
}

fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "smallint"
            | "int2"
            | "pg_catalog.int2"
            | "integer"
            | "int"
            | "int4"
            | "serial"
            | "serial4"
            | "pg_catalog.int4"
            | "bigint"
            | "int8"
            | "bigserial"
            | "serial8"
            | "pg_catalog.int8"
    )
}

fn optimize_frame_bound(bound: &mut ScalarFrameBound, config: &OptimizerConfig) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            optimize_scalar_slot(expression, config);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}

fn simplify_and(items: Vec<ScalarExpr>) -> ScalarExpr {
    let mut kept = Vec::new();
    for item in items {
        match item {
            ScalarExpr::Literal(Value::Bool(true)) => {}
            ScalarExpr::Literal(Value::Bool(false)) => {
                return ScalarExpr::Literal(Value::Bool(false));
            }
            ScalarExpr::And(inner) => kept.extend(inner),
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        ScalarExpr::Literal(Value::Bool(true))
    } else if kept.len() == 1 {
        kept.remove(0)
    } else {
        ScalarExpr::And(kept)
    }
}

fn simplify_or(items: Vec<ScalarExpr>) -> ScalarExpr {
    let mut kept = Vec::new();
    for item in items {
        match item {
            ScalarExpr::Literal(Value::Bool(false)) => {}
            ScalarExpr::Literal(Value::Bool(true)) => {
                return ScalarExpr::Literal(Value::Bool(true));
            }
            ScalarExpr::Or(inner) => kept.extend(inner),
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        ScalarExpr::Literal(Value::Bool(false))
    } else if kept.len() == 1 {
        kept.remove(0)
    } else {
        ScalarExpr::Or(kept)
    }
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
    fn folds_literal_date_cast_before_row_execution() {
        let mut expression = ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Str("1993-07-01".into()))),
            ty: "date".into(),
        };

        optimize_scalar_slot(&mut expression, &OptimizerConfig::default());

        assert!(matches!(
            expression,
            ScalarExpr::Literal(Value::Temporal(_))
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

        optimize_scalar_slot(&mut expression, &OptimizerConfig::default());

        assert_eq!(expression, ScalarExpr::Literal(Value::Int(20)));
    }

    #[test]
    fn leaves_invalid_literal_cast_for_runtime_error_reporting() {
        let mut expression = ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Str("not-a-date".into()))),
            ty: "date".into(),
        };

        optimize_scalar_slot(&mut expression, &OptimizerConfig::default());

        assert!(matches!(expression, ScalarExpr::Cast { .. }));
    }
}
