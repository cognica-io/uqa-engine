//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar Boolean simplification and vector-threshold rewrites.

use super::{
    AssignmentPlan, BTreeMap, OptimizerConfig, ProjectionPlan, ScalarExpr, ScalarFrameBound, Value,
};

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
    match expression {
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
    }
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
