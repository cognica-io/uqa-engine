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
        self.try_visit(&mut |expression| {
            visitor(expression);
            Ok::<_, std::convert::Infallible>(true)
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit in pre-order, skipping a subtree when the visitor returns false and stopping immediately on its first error.
    pub fn try_visit<E>(
        &self,
        visitor: &mut impl FnMut(&Self) -> Result<bool, E>,
    ) -> Result<(), E> {
        if !visitor(self)? {
            return Ok(());
        }
        match self {
            Self::And(parts) | Self::Or(parts) | Self::Array(parts) | Self::Row(parts) => {
                for part in parts {
                    part.try_visit(visitor)?;
                }
            }
            Self::Not(inner)
            | Self::UnaryMinus(inner)
            | Self::Cast { expr: inner, .. }
            | Self::IsNull { expr: inner, .. }
            | Self::InSubquery { expr: inner, .. } => inner.try_visit(visitor)?,
            Self::Binary { lhs, rhs, .. } => {
                lhs.try_visit(visitor)?;
                rhs.try_visit(visitor)?;
            }
            Self::Between { expr, low, high } => {
                expr.try_visit(visitor)?;
                low.try_visit(visitor)?;
                high.try_visit(visitor)?;
            }
            Self::InList { expr, list, .. } => {
                expr.try_visit(visitor)?;
                for part in list {
                    part.try_visit(visitor)?;
                }
            }
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                for argument in args {
                    argument.try_visit(visitor)?;
                }
                for order in order_by {
                    order.expr.try_visit(visitor)?;
                }
                if let Some(filter) = filter {
                    filter.try_visit(visitor)?;
                }
            }
            Self::WindowCall { args, spec, .. } => {
                for argument in args {
                    argument.try_visit(visitor)?;
                }
                for partition in &spec.partition_by {
                    partition.try_visit(visitor)?;
                }
                for order in &spec.order_by {
                    order.expr.try_visit(visitor)?;
                }
                if let Some(frame) = &spec.frame {
                    for bound in [&frame.start, &frame.end] {
                        match bound {
                            ScalarFrameBound::Preceding(expression)
                            | ScalarFrameBound::Following(expression) => {
                                expression.try_visit(visitor)?;
                            }
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
                    base.try_visit(visitor)?;
                }
                for (condition, result) in when {
                    condition.try_visit(visitor)?;
                    result.try_visit(visitor)?;
                }
                if let Some(else_branch) = else_branch {
                    else_branch.try_visit(visitor)?;
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
            | Self::TypedLiteral { .. }
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => {}
        }
        Ok(())
    }

    /// Collect every column needed to evaluate this expression. Returns `false` when evaluation needs row shape or a relational child that a projected field scan cannot provide.
    pub fn collect_columns(&self, output: &mut std::collections::BTreeSet<String>) -> bool {
        match self.try_visit_columns(&mut |name| {
            output.insert(name.to_owned());
            Ok::<_, std::convert::Infallible>(())
        }) {
            Ok(projectable) => projectable,
            Err(never) => match never {},
        }
    }

    /// Borrow referenced column names in evaluation-tree order, stopping at the first visitor failure or unprojectable expression. Qualified references yield their column component, matching `collect_columns`; repeated references remain visible to the visitor. No name or result container is allocated by this traversal.
    pub fn try_visit_columns<'a, E>(
        &'a self,
        visitor: &mut impl FnMut(&'a str) -> Result<(), E>,
    ) -> Result<bool, E> {
        match self {
            Self::Column(name) | Self::QualifiedColumn { column: name, .. } => {
                visitor(name)?;
                Ok(true)
            }
            Self::Literal(_)
            | Self::TypedLiteral { .. }
            | Self::Param(_)
            | Self::InternalColumn(_) => Ok(true),
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                for expression in args
                    .iter()
                    .chain(order_by.iter().map(|order| &order.expr))
                    .chain(filter.as_deref())
                {
                    if !expression.try_visit_columns(visitor)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                for item in items {
                    if !item.try_visit_columns(visitor)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Binary { lhs, rhs, .. } => {
                Ok(lhs.try_visit_columns(visitor)? && rhs.try_visit_columns(visitor)?)
            }
            Self::UnaryMinus(expr)
            | Self::Not(expr)
            | Self::IsNull { expr, .. }
            | Self::Cast { expr, .. } => expr.try_visit_columns(visitor),
            Self::Between { expr, low, high } => Ok(expr.try_visit_columns(visitor)?
                && low.try_visit_columns(visitor)?
                && high.try_visit_columns(visitor)?),
            Self::InList { expr, list, .. } => {
                for item in std::iter::once(expr.as_ref()).chain(list) {
                    if !item.try_visit_columns(visitor)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                for expression in base
                    .as_deref()
                    .into_iter()
                    .chain(
                        when.iter()
                            .flat_map(|(condition, result)| [condition, result]),
                    )
                    .chain(else_branch.as_deref())
                {
                    if !expression.try_visit_columns(visitor)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Position(_)
            | Self::WindowCall { .. }
            | Self::ScalarSubquery(_)
            | Self::Exists { .. }
            | Self::InSubquery { .. } => Ok(false),
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
            | Self::TypedLiteral { .. }
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
            | Self::TypedLiteral { .. }
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
            | Self::TypedLiteral { .. }
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
            | Self::TypedLiteral { .. }
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
    use crate::ast::FrameMode;
    use uqa_core::Value;

    #[test]
    fn visit_includes_root_and_nested_expressions() {
        let expression = ScalarExpr::Binary {
            op: crate::ast::BinaryOp::Add,
            lhs: Box::new(ScalarExpr::Column("amount".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        };
        let mut visited = Vec::new();
        expression.visit(&mut |part| visited.push(part.clone()));
        assert_eq!(visited.len(), 3);
        assert_eq!(visited[0], expression);
    }

    #[test]
    fn fallible_visits_skip_selected_subtrees_and_stop_before_later_siblings() {
        let expression = ScalarExpr::Row(vec![
            ScalarExpr::Array(vec![ScalarExpr::Column("hidden".into())]),
            ScalarExpr::Column("reject".into()),
            ScalarExpr::Column("unvisited".into()),
        ]);
        let mut visited = Vec::new();
        let result = expression.try_visit(&mut |part| {
            visited.push(part.clone());
            match part {
                ScalarExpr::Array(_) => Ok(false),
                ScalarExpr::Column(name) if name == "reject" => Err("grouping"),
                _ => Ok(true),
            }
        });
        assert_eq!(result, Err("grouping"));
        assert_eq!(visited.len(), 3);
        assert!(matches!(&visited[2], ScalarExpr::Column(name) if name == "reject"));
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

    #[test]
    fn borrowed_column_visits_keep_names_and_stop_at_the_first_failure() {
        let expression = ScalarExpr::Row(vec![
            ScalarExpr::Column("first".into()),
            ScalarExpr::QualifiedColumn {
                qualifier: "table".into(),
                column: "second".into(),
            },
            ScalarExpr::Column("first".into()),
        ]);
        let mut borrowed = Vec::new();
        assert!(expression
            .try_visit_columns(&mut |name| {
                borrowed.push(name);
                Ok::<_, &str>(())
            })
            .unwrap());
        assert_eq!(borrowed, ["first", "second", "first"]);
        let ScalarExpr::Row(items) = &expression else {
            unreachable!()
        };
        let ScalarExpr::Column(first) = &items[0] else {
            unreachable!()
        };
        assert_eq!(borrowed[0].as_ptr(), first.as_ptr());
        let mut visits = 0;
        let result = expression.try_visit_columns(&mut |_| {
            visits += 1;
            if visits == 2 {
                Err("quota")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("quota"));
        assert_eq!(visits, 2);
    }

    #[test]
    fn borrowed_column_visits_preserve_unprojectable_prefix_semantics() {
        let expression = ScalarExpr::Array(vec![
            ScalarExpr::Column("before".into()),
            ScalarExpr::Position(0),
            ScalarExpr::Column("after".into()),
        ]);
        let mut borrowed = Vec::new();
        assert!(!expression
            .try_visit_columns(&mut |name| {
                borrowed.push(name);
                Ok::<_, &str>(())
            })
            .unwrap());
        let mut owned = std::collections::BTreeSet::new();
        assert!(!expression.collect_columns(&mut owned));
        assert_eq!(borrowed, ["before"]);
        assert_eq!(owned, std::collections::BTreeSet::from(["before".into()]));
    }
}
