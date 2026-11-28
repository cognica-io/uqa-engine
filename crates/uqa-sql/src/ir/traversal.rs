//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete scalar IR traversal.

use super::ScalarExpr;

#[cfg(test)]
std::thread_local! {
    static VISITED_NODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ScalarExpr {
    #[cfg(test)]
    pub(crate) fn visited_nodes() -> usize {
        VISITED_NODES.get()
    }

    /// Visit this expression and every nested scalar expression in pre-order.
    pub fn visit(&self, visitor: &mut impl FnMut(&Self)) {
        self.try_visit(&mut |expression| {
            visitor(expression);
            Ok::<_, std::convert::Infallible>(true)
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit in pre-order, skipping a subtree when the visitor returns false and stopping immediately on its first error.
    pub fn try_visit<'a, E>(
        &'a self,
        visitor: &mut impl FnMut(&'a Self) -> Result<bool, E>,
    ) -> Result<(), E> {
        #[cfg(test)]
        VISITED_NODES.set(VISITED_NODES.get() + 1);
        if visitor(self)? {
            for child in self.children(super::children::Scope::Complete) {
                child.try_visit(visitor)?;
            }
        }
        Ok(())
    }

    /// Visit each borrowed node after its children, stopping at the first error.
    pub fn try_visit_post<'a, E>(
        &'a self,
        visitor: &mut impl FnMut(&'a Self) -> Result<(), E>,
    ) -> Result<(), E> {
        for child in self.children(super::children::Scope::Complete) {
            child.try_visit_post(visitor)?;
        }
        visitor(self)
    }

    /// Visit this expression and every nested scalar expression mutably in pre-order: a node is visited before its children, so the children of a node the visitor replaces are those of the replacement.
    pub fn visit_mut(&mut self, visitor: &mut impl FnMut(&mut Self)) {
        self.try_visit_mut(&mut |expression| {
            visitor(expression);
            Ok::<_, std::convert::Infallible>(true)
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit mutable nodes in pre-order. Returning false skips the current children. An error skips all remaining nodes.
    pub fn try_visit_mut<E>(
        &mut self,
        visitor: &mut impl FnMut(&mut Self) -> Result<bool, E>,
    ) -> Result<(), E> {
        if visitor(self)? {
            for child in self.children_mut(super::children::Scope::Complete) {
                child.try_visit_mut(visitor)?;
            }
        }
        Ok(())
    }

    /// Visit each mutable node after its original children, stopping at the first error. Children introduced by a replacement are left to its caller.
    pub fn try_visit_post_mut<E>(
        &mut self,
        visitor: &mut impl FnMut(&mut Self) -> Result<(), E>,
    ) -> Result<(), E> {
        for child in self.children_mut(super::children::Scope::Complete) {
            child.try_visit_post_mut(visitor)?;
        }
        visitor(self)
    }

    /// Whether any scalar node satisfies the predicate, in pre-order.
    pub fn any_node(&self, mut predicate: impl FnMut(&Self) -> bool) -> bool {
        self.try_any_node(|node| Ok::<_, std::convert::Infallible>(predicate(node)))
            .unwrap_or_else(|never| match never {})
    }

    /// Stop at the first matching node or predicate failure without descending into remaining children.
    pub fn try_any_node<E>(
        &self,
        mut predicate: impl FnMut(&Self) -> Result<bool, E>,
    ) -> Result<bool, E> {
        match self.try_visit(&mut |node| match predicate(node) {
            Ok(true) => Err(None),
            Ok(false) => Ok(true),
            Err(error) => Err(Some(error)),
        }) {
            Ok(()) => Ok(false),
            Err(None) => Ok(true),
            Err(Some(error)) => Err(error),
        }
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
        match self.try_visit(&mut |expression| {
            match expression {
                Self::Column(name) | Self::QualifiedColumn { column: name, .. } => {
                    visitor(name).map_err(Some)?;
                }
                Self::Default
                | Self::Star
                | Self::QualifiedStar(_)
                | Self::Position(_)
                | Self::WindowCall { .. }
                | Self::ScalarSubquery(_)
                | Self::Exists { .. }
                | Self::InSubquery { .. } => return Err(None),
                _ => (),
            }
            Ok(true)
        }) {
            Ok(()) => Ok(true),
            Err(None) => Ok(false),
            Err(Some(error)) => Err(error),
        }
    }

    #[must_use]
    pub fn contains_window(&self) -> bool {
        crate::catalog::work::inline(self.contains_window_with_work(
            &uqa_core::memory::ProductionControl::uncontrolled(),
            &mut (),
        ))
        .expect("ordinary window observation")
    }

    pub(crate) async fn contains_window_with_work<W: crate::catalog::work::Work>(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, crate::SQLError> {
        use super::observation::Decision;
        super::observation::visit(self, control, work, true, &mut |expression| {
            Ok(if matches!(expression, Self::WindowCall { .. }) {
                Decision::Stop
            } else {
                Decision::Descend
            })
        })
        .await
    }

    #[must_use]
    pub fn contains_subquery(&self) -> bool {
        self.any_node(|node| {
            matches!(
                node,
                Self::ScalarSubquery(_) | Self::Exists { .. } | Self::InSubquery { .. }
            )
        })
    }

    #[must_use]
    pub fn contains_parameter(&self) -> bool {
        self.any_node(|node| matches!(node, Self::Param(_)))
    }

    #[must_use]
    pub fn contains_aggregate(&self, is_aggregate: &dyn Fn(&str) -> bool) -> bool {
        self.try_visit(&mut |node| match node {
            Self::Func { name, .. } if is_aggregate(name) => Err(()),
            Self::WindowCall { .. } => Ok(false),
            _ => Ok(true),
        })
        .is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::ScalarExpr;
    use crate::ast::{FrameExclusion, FrameMode};
    use crate::ScalarFrameBound;
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
    fn fallible_mutable_walkers_preserve_replacement_order_and_stop_at_errors() {
        let mut expression = ScalarExpr::Row(vec![
            ScalarExpr::Param(0),
            ScalarExpr::Param(1),
            ScalarExpr::Param(2),
        ]);
        let mut reached = Vec::new();
        expression
            .try_visit_mut(&mut |node| {
                if let ScalarExpr::Param(number) = node {
                    reached.push(*number);
                    if *number < 2 {
                        let old = *number;
                        *node = ScalarExpr::Array(vec![ScalarExpr::Param(10 + old)]);
                        return Ok::<_, ()>(old == 0);
                    }
                }
                Ok(true)
            })
            .unwrap();
        assert_eq!(reached, [0, 10, 1, 2]);
        reached.clear();
        let result = expression.try_visit_post_mut(&mut |node| {
            match node {
                ScalarExpr::Param(number) => reached.push(*number),
                ScalarExpr::Array(_) if reached == [10] => {
                    *node = ScalarExpr::Param(20);
                    reached.push(20);
                }
                ScalarExpr::Array(_) => return Err("stop"),
                _ => (),
            }
            Ok(())
        });
        assert_eq!(result, Err("stop"));
        assert_eq!(reached, [10, 20, 11]);
    }

    #[test]
    fn traversal_includes_window_frame_expressions() {
        let expression = ScalarExpr::WindowCall {
            name: "sum".into(),
            args: vec![ScalarExpr::Column("amount".into())],
            spec: super::super::ScalarWindowSpec {
                definition: None,
                partition_by: vec![ScalarExpr::QualifiedColumn {
                    qualifier: "orders".into(),
                    column: "account_id".into(),
                }],
                order_by: Vec::new(),
                frame: Some(super::super::ScalarWindowFrame {
                    mode: FrameMode::Rows,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Param(0))),
                    end: ScalarFrameBound::CurrentRow,
                    between: true,
                    exclusion: FrameExclusion::NoOthers,
                }),
            },
            filter: None,
            modifiers: crate::ast::WindowCallModifiers::default(),
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
    fn mutable_visits_rewrite_nested_expressions() {
        let literal = || ScalarExpr::Literal(Value::Str("t".into()));
        let mut expression = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Case {
                base: None,
                when: vec![(
                    literal(),
                    ScalarExpr::Cast {
                        implicit: false,
                        expr: Box::new(literal()),
                        ty: "regclass".into(),
                    },
                )],
                else_branch: Some(Box::new(ScalarExpr::WindowCall {
                    name: "sum".into(),
                    args: vec![literal()],
                    spec: super::super::ScalarWindowSpec {
                        definition: None,
                        partition_by: Vec::new(),
                        order_by: Vec::new(),
                        frame: Some(super::super::ScalarWindowFrame {
                            mode: FrameMode::Rows,
                            start: ScalarFrameBound::Preceding(Box::new(literal())),
                            end: ScalarFrameBound::CurrentRow,
                            between: true,
                            exclusion: FrameExclusion::NoOthers,
                        }),
                    },
                    filter: Some(Box::new(literal())),
                    modifiers: crate::ast::WindowCallModifiers::default(),
                })),
            }),
            ty: "text".into(),
        };
        let mut rewritten = 0;
        expression.visit_mut(&mut |part| {
            if matches!(part, ScalarExpr::Literal(Value::Str(_))) {
                *part = ScalarExpr::Literal(Value::Int(1));
                rewritten += 1;
            }
        });
        assert_eq!(rewritten, 5);
        let mut remaining = 0;
        expression.visit(&mut |part| {
            remaining += usize::from(matches!(part, ScalarExpr::Literal(Value::Str(_))));
        });
        assert_eq!(remaining, 0);
    }

    #[test]
    fn owned_walkers_preserve_column_and_aggregate_policy() {
        let expression = ScalarExpr::Func {
            order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
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
