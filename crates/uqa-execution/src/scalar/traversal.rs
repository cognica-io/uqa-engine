//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete scalar IR traversal.

use super::{ScalarExpr, ScalarFrameBound};

impl ScalarExpr {
    /// Visit this expression and every nested scalar expression in pre-order.
    pub fn visit(&self, visitor: &mut impl FnMut(&Self)) {
        visitor(self);
        match self {
            Self::And(parts) | Self::Or(parts) | Self::Array(parts) | Self::Row(parts) => {
                for part in parts {
                    part.visit(visitor);
                }
            }
            Self::Not(inner)
            | Self::UnaryMinus(inner)
            | Self::Cast { expr: inner, .. }
            | Self::IsNull { expr: inner, .. }
            | Self::InSubquery { expr: inner, .. } => inner.visit(visitor),
            Self::Binary { lhs, rhs, .. } => {
                lhs.visit(visitor);
                rhs.visit(visitor);
            }
            Self::Between { expr, low, high } => {
                expr.visit(visitor);
                low.visit(visitor);
                high.visit(visitor);
            }
            Self::InList { expr, list, .. } => {
                expr.visit(visitor);
                for part in list {
                    part.visit(visitor);
                }
            }
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                for argument in args {
                    argument.visit(visitor);
                }
                for order in order_by {
                    order.expr.visit(visitor);
                }
                if let Some(filter) = filter {
                    filter.visit(visitor);
                }
            }
            Self::WindowCall { args, spec, .. } => {
                for argument in args {
                    argument.visit(visitor);
                }
                for partition in &spec.partition_by {
                    partition.visit(visitor);
                }
                for order in &spec.order_by {
                    order.expr.visit(visitor);
                }
                if let Some(frame) = &spec.frame {
                    for bound in [&frame.start, &frame.end] {
                        match bound {
                            ScalarFrameBound::Preceding(expression)
                            | ScalarFrameBound::Following(expression) => expression.visit(visitor),
                            ScalarFrameBound::UnboundedPreceding
                            | ScalarFrameBound::UnboundedFollowing
                            | ScalarFrameBound::CurrentRow => {}
                        }
                    }
                }
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                if let Some(base) = base {
                    base.visit(visitor);
                }
                for (condition, result) in when {
                    condition.visit(visitor);
                    result.visit(visitor);
                }
                if let Some(else_branch) = else_branch {
                    else_branch.visit(visitor);
                }
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::Position(_)
            | Self::InternalColumn(_)
            | Self::QualifiedColumn { .. }
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => {}
        }
    }

    /// Collect every column needed to evaluate this expression. Returns `false` when evaluation needs row shape or a relational child that a projected field scan cannot provide.
    pub fn collect_columns(&self, output: &mut std::collections::BTreeSet<String>) -> bool {
        match self {
            Self::Column(name) | Self::QualifiedColumn { column: name, .. } => {
                output.insert(name.clone());
                true
            }
            Self::Literal(_) | Self::Param(_) | Self::InternalColumn(_) => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().all(|arg| arg.collect_columns(output))
                    && order_by
                        .iter()
                        .all(|order| order.expr.collect_columns(output))
                    && filter
                        .as_deref()
                        .is_none_or(|filter| filter.collect_columns(output))
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().all(|item| item.collect_columns(output))
            }
            Self::Binary { lhs, rhs, .. } => {
                lhs.collect_columns(output) && rhs.collect_columns(output)
            }
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. } => expr.collect_columns(output),
            Self::Between { expr, low, high } => {
                expr.collect_columns(output)
                    && low.collect_columns(output)
                    && high.collect_columns(output)
            }
            Self::InList { expr, list, .. } => {
                expr.collect_columns(output) && list.iter().all(|item| item.collect_columns(output))
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref()
                    .is_none_or(|base| base.collect_columns(output))
                    && when.iter().all(|(condition, result)| {
                        condition.collect_columns(output) && result.collect_columns(output)
                    })
                    && else_branch
                        .as_deref()
                        .is_none_or(|branch| branch.collect_columns(output))
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Position(_)
            | Self::WindowCall { .. }
            | Self::ScalarSubquery(_)
            | Self::Exists { .. }
            | Self::InSubquery { .. } => false,
        }
    }

    #[must_use]
    pub fn contains_window(&self) -> bool {
        match self {
            Self::WindowCall { .. } => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(Self::contains_window)
                    || order_by.iter().any(|order| order.expr.contains_window())
                    || filter.as_deref().is_some_and(Self::contains_window)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_window)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_window() || rhs.contains_window(),
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. }
            | Self::InSubquery { expr, .. } => expr.contains_window(),
            Self::Between { expr, low, high } => {
                expr.contains_window() || low.contains_window() || high.contains_window()
            }
            Self::InList { expr, list, .. } => {
                expr.contains_window() || list.iter().any(Self::contains_window)
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(Self::contains_window)
                    || when.iter().any(|(condition, result)| {
                        condition.contains_window() || result.contains_window()
                    })
                    || else_branch.as_deref().is_some_and(Self::contains_window)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => false,
        }
    }

    #[must_use]
    pub fn contains_subquery(&self) -> bool {
        match self {
            Self::ScalarSubquery(_) | Self::Exists { .. } | Self::InSubquery { .. } => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(Self::contains_subquery)
                    || order_by.iter().any(|order| order.expr.contains_subquery())
                    || filter.as_deref().is_some_and(Self::contains_subquery)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_subquery)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_subquery() || rhs.contains_subquery(),
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. } => expr.contains_subquery(),
            Self::Between { expr, low, high } => {
                expr.contains_subquery() || low.contains_subquery() || high.contains_subquery()
            }
            Self::InList { expr, list, .. } => {
                expr.contains_subquery() || list.iter().any(Self::contains_subquery)
            }
            Self::WindowCall { args, spec, .. } => {
                args.iter().any(Self::contains_subquery)
                    || spec.partition_by.iter().any(Self::contains_subquery)
                    || spec
                        .order_by
                        .iter()
                        .any(|order| order.expr.contains_subquery())
                    || spec.frame.as_ref().is_some_and(|frame| {
                        frame_has(&frame.start, Self::contains_subquery)
                            || frame_has(&frame.end, Self::contains_subquery)
                    })
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(Self::contains_subquery)
                    || when.iter().any(|(condition, result)| {
                        condition.contains_subquery() || result.contains_subquery()
                    })
                    || else_branch.as_deref().is_some_and(Self::contains_subquery)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::Param(_) => false,
        }
    }

    #[must_use]
    pub fn contains_parameter(&self) -> bool {
        match self {
            Self::Param(_) => true,
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(Self::contains_parameter)
                    || order_by.iter().any(|order| order.expr.contains_parameter())
                    || filter.as_deref().is_some_and(Self::contains_parameter)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(Self::contains_parameter)
            }
            Self::Binary { lhs, rhs, .. } => lhs.contains_parameter() || rhs.contains_parameter(),
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. }
            | Self::InSubquery { expr, .. } => expr.contains_parameter(),
            Self::Between { expr, low, high } => {
                expr.contains_parameter() || low.contains_parameter() || high.contains_parameter()
            }
            Self::InList { expr, list, .. } => {
                expr.contains_parameter() || list.iter().any(Self::contains_parameter)
            }
            Self::WindowCall { args, spec, .. } => {
                args.iter().any(Self::contains_parameter)
                    || spec.partition_by.iter().any(Self::contains_parameter)
                    || spec
                        .order_by
                        .iter()
                        .any(|order| order.expr.contains_parameter())
                    || spec.frame.as_ref().is_some_and(|frame| {
                        frame_has(&frame.start, Self::contains_parameter)
                            || frame_has(&frame.end, Self::contains_parameter)
                    })
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(Self::contains_parameter)
                    || when.iter().any(|(condition, result)| {
                        condition.contains_parameter() || result.contains_parameter()
                    })
                    || else_branch.as_deref().is_some_and(Self::contains_parameter)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => false,
        }
    }

    #[must_use]
    pub fn contains_aggregate(&self, is_aggregate: &dyn Fn(&str) -> bool) -> bool {
        match self {
            Self::Func {
                name,
                args,
                order_by,
                filter,
                ..
            } => {
                is_aggregate(name)
                    || args
                        .iter()
                        .any(|expression| expression.contains_aggregate(is_aggregate))
                    || order_by
                        .iter()
                        .any(|order| order.expr.contains_aggregate(is_aggregate))
                    || filter
                        .as_deref()
                        .is_some_and(|expression| expression.contains_aggregate(is_aggregate))
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => items
                .iter()
                .any(|expression| expression.contains_aggregate(is_aggregate)),
            Self::Binary { lhs, rhs, .. } => {
                lhs.contains_aggregate(is_aggregate) || rhs.contains_aggregate(is_aggregate)
            }
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. }
            | Self::InSubquery { expr, .. } => expr.contains_aggregate(is_aggregate),
            Self::Between { expr, low, high } => {
                expr.contains_aggregate(is_aggregate)
                    || low.contains_aggregate(is_aggregate)
                    || high.contains_aggregate(is_aggregate)
            }
            Self::InList { expr, list, .. } => {
                expr.contains_aggregate(is_aggregate)
                    || list
                        .iter()
                        .any(|item| item.contains_aggregate(is_aggregate))
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref()
                    .is_some_and(|expression| expression.contains_aggregate(is_aggregate))
                    || when.iter().any(|(condition, result)| {
                        condition.contains_aggregate(is_aggregate)
                            || result.contains_aggregate(is_aggregate)
                    })
                    || else_branch
                        .as_deref()
                        .is_some_and(|expression| expression.contains_aggregate(is_aggregate))
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Position(_)
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. }
            | Self::WindowCall { .. } => false,
        }
    }
}

fn frame_has(bound: &ScalarFrameBound, predicate: fn(&ScalarExpr) -> bool) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            predicate(expression)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{ScalarExpr, ScalarFrameBound};
    use uqa_core::Value;
    use uqa_sql::ast::FrameMode;

    #[test]
    fn visit_includes_root_and_nested_expressions() {
        let expression = ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Add,
            lhs: Box::new(ScalarExpr::Column("amount".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        };
        let mut visited = Vec::new();
        expression.visit(&mut |part| visited.push(part.clone()));
        assert_eq!(visited.len(), 3);
        assert_eq!(visited[0], expression);
    }

    #[test]
    fn traversal_includes_window_frame_expressions() {
        let expression = ScalarExpr::WindowCall {
            name: "sum".into(),
            args: vec![ScalarExpr::Column("amount".into())],
            spec: super::super::ScalarWindowSpec {
                partition_by: vec![ScalarExpr::QualifiedColumn {
                    qualifier: "orders".into(),
                    column: "account_id".into(),
                }],
                order_by: Vec::new(),
                frame: Some(super::super::ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Param(0))),
                    end: ScalarFrameBound::CurrentRow,
                }),
            },
        };
        let mut visited_parameter = false;
        expression.visit(&mut |part| {
            visited_parameter |= matches!(part, ScalarExpr::Param(0));
        });
        assert!(visited_parameter);
        assert!(expression.contains_window());
        assert!(expression.contains_parameter());
    }

    #[test]
    fn owned_walkers_preserve_column_and_aggregate_policy() {
        let expression = ScalarExpr::Func {
            name: "sum".into(),
            binding: None,
            args: vec![ScalarExpr::QualifiedColumn {
                qualifier: "orders".into(),
                column: "amount".into(),
            }],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let mut columns = std::collections::BTreeSet::new();
        assert!(expression.collect_columns(&mut columns));
        assert_eq!(columns, std::collections::BTreeSet::from(["amount".into()]));
        assert!(expression.contains_aggregate(&|name| name == "sum"));
        assert!(!expression.contains_subquery());
    }
}
