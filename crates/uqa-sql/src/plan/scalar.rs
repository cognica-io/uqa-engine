//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL AST to executable scalar IR lowering and aggregate classification.

use super::{
    AggregateClassifier, Expr, FrameBound, OrderBy, QueryPlan, ScalarExpr, ScalarFrameBound,
    ScalarOrder, ScalarWindowFrame, ScalarWindowSpec, WindowSpec,
};
use crate::schema::retention::CatalogRetentionError;
use resources::{Control, Lowering, Result};
use source::{Node, Source};
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

mod binding;
mod resources;
mod source;
mod window;

impl super::ExpressionPlan {
    /// Lower a borrowed, validated column expression directly into admitted scalar IR. Destination strings, value payloads, bindings, vector capacities and boxes acquire the supplied allowance before allocation, and the result retains those leases. Both the retained definition's original cancellation and the invoking reader's cancellation remain active during lowering. This controls AST-to-IR production only; subsequent type binding and evaluation require their own resource contracts. Query children violate the validated column-expression invariant.
    pub fn lower_column_budgeted(
        expression: &Expr,
        budget: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> Result<Budgeted<ScalarExpr>> {
        let mut lowering = Lowering {
            control: Some(Control::new(budget, original, invoking)),
        };
        let scalar = lowering.expression(
            Source::Borrowed(expression),
            &super::NoRegisteredAggregates,
            &mut Vec::new(),
        )?;
        lowering.finish(scalar)
    }
}

pub(super) fn lower_scalar_expression(
    expression: Expr,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarExpr {
    Lowering { control: None }
        .expression(Source::Owned(expression), aggregates, subqueries)
        .expect("owned lowering has no admission failure")
}

impl Lowering<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "plan lowering preserves exhaustive variants and structural identities"
    )]
    fn expression(
        &mut self,
        expression: Source<'_, Expr>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<ScalarExpr> {
        self.check()?;
        Ok(match expression.node() {
            Node::Star => ScalarExpr::Star,
            Node::QualifiedStar(name) => ScalarExpr::QualifiedStar(self.text(name)?),
            Node::Default => ScalarExpr::Default,
            Node::Column(name) => ScalarExpr::Column(self.text(name)?),
            Node::QualifiedColumn { qualifier, column } => ScalarExpr::QualifiedColumn {
                qualifier: self.text(qualifier)?,
                column: self.text(column)?,
            },
            Node::InternalColumn(column) => ScalarExpr::InternalColumn(column),
            Node::Literal(value) => ScalarExpr::Literal(self.value(value)?),
            Node::TypedLiteral { value, ty } => ScalarExpr::TypedLiteral {
                value: self.value(value)?,
                ty: self.text(ty)?,
                bound_type: None,
                parameter_index: None,
            },
            Node::Param(index) => ScalarExpr::Param(index),
            Node::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                filter,
            } => ScalarExpr::Func {
                name: self.text(name)?,
                binding: binding.map(|binding| self.binding(binding)).transpose()?,
                args: self.map(args, |this, argument| {
                    this.expression(argument, aggregates, subqueries)
                })?,
                distinct,
                order_by: self.map(order_by, |this, order| {
                    this.order(order, aggregates, subqueries)
                })?,
                filter: filter
                    .map(|filter| self.child(filter, aggregates, subqueries))
                    .transpose()?,
            },
            Node::Array(items) => ScalarExpr::Array(self.map(items, |this, item| {
                this.expression(item, aggregates, subqueries)
            })?),
            Node::Row(items) => ScalarExpr::Row(self.map(items, |this, item| {
                this.expression(item, aggregates, subqueries)
            })?),
            Node::Binary { op, lhs, rhs } => ScalarExpr::Binary {
                op,
                lhs: self.child(lhs, aggregates, subqueries)?,
                rhs: self.child(rhs, aggregates, subqueries)?,
            },
            Node::UnaryMinus(expression) => {
                ScalarExpr::UnaryMinus(self.child(expression, aggregates, subqueries)?)
            }
            Node::Not(expression) => {
                ScalarExpr::Not(self.child(expression, aggregates, subqueries)?)
            }
            Node::And(items) => ScalarExpr::And(self.map(items, |this, item| {
                this.expression(item, aggregates, subqueries)
            })?),
            Node::Or(items) => ScalarExpr::Or(self.map(items, |this, item| {
                this.expression(item, aggregates, subqueries)
            })?),
            Node::IsNull { expr, negated } => ScalarExpr::IsNull {
                expr: self.child(expr, aggregates, subqueries)?,
                negated,
            },
            Node::Between { expr, low, high } => ScalarExpr::Between {
                expr: self.child(expr, aggregates, subqueries)?,
                low: self.child(low, aggregates, subqueries)?,
                high: self.child(high, aggregates, subqueries)?,
            },
            Node::InList {
                expr,
                list,
                negated,
            } => ScalarExpr::InList {
                expr: self.child(expr, aggregates, subqueries)?,
                list: self.map(list, |this, item| {
                    this.expression(item, aggregates, subqueries)
                })?,
                negated,
            },
            Node::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
                name: self.text(name)?,
                args: self.map(args, |this, argument| {
                    this.expression(argument, aggregates, subqueries)
                })?,
                spec: self.window(spec, aggregates, subqueries)?,
            },
            Node::Case {
                base,
                when,
                else_branch,
            } => ScalarExpr::Case {
                base: base
                    .map(|base| self.child(base, aggregates, subqueries))
                    .transpose()?,
                when: self.map(when, |this, pair| {
                    let (condition, result) = pair.pair();
                    Ok((
                        this.expression(condition, aggregates, subqueries)?,
                        this.expression(result, aggregates, subqueries)?,
                    ))
                })?,
                else_branch: else_branch
                    .map(|branch| self.child(branch, aggregates, subqueries))
                    .transpose()?,
            },
            Node::Cast { expr, ty } => ScalarExpr::Cast {
                expr: self.child(expr, aggregates, subqueries)?,
                ty: self.text(ty)?,
            },
            Node::ScalarSubquery(query) => {
                ScalarExpr::ScalarSubquery(self.query(query, aggregates, subqueries)?)
            }
            Node::Exists { body, negated } => ScalarExpr::Exists {
                subquery: self.query(body, aggregates, subqueries)?,
                negated,
            },
            Node::InSubquery {
                expr,
                body,
                negated,
            } => {
                let expr = self.child(expr, aggregates, subqueries)?;
                ScalarExpr::InSubquery {
                    expr,
                    subquery: self.query(body, aggregates, subqueries)?,
                    negated,
                }
            }
        })
    }

    fn child(
        &mut self,
        expression: Source<'_, Box<Expr>>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<Box<ScalarExpr>> {
        self.boxed(|this| this.expression(expression.unbox(), aggregates, subqueries))
    }

    fn query(
        &self,
        query: Source<'_, Box<crate::ast::SelectStmt>>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<usize> {
        self.check()?;
        let Source::Owned(query) = query else {
            return Err(CatalogRetentionError::UnexpectedSubquery);
        };
        let id = subqueries.len();
        subqueries.push(QueryPlan::lower_with(*query, aggregates));
        Ok(id)
    }
}

pub(crate) fn is_builtin_aggregate(name: &str) -> bool {
    crate::ast::is_builtin_aggregate_function(name)
}

#[cfg(test)]
mod tests;
