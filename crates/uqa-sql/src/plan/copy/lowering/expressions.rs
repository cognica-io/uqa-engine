//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result};
use crate::{
    ast::{Expr, OrderBy},
    plan::{AggregateClassifier, ExpressionPlan, OrderPlan},
    ScalarExpr,
};

impl Copying<'_> {
    pub(super) fn lower_optional_scalar(
        &mut self,
        expression: Option<&Expr>,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Option<ScalarExpr>> {
        expression
            .map(|expression| self.lower_scalar(expression, aggregates, arena))
            .transpose()
    }

    pub(super) fn lower_scalar_box(
        &mut self,
        expression: &Expr,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Box<ScalarExpr>> {
        self.boxed(|copying| copying.lower_scalar(expression, aggregates, arena))
    }

    pub(super) fn lower_optional_scalar_box(
        &mut self,
        expression: Option<&Expr>,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Option<Box<ScalarExpr>>> {
        expression
            .map(|expression| self.lower_scalar_box(expression, aggregates, arena))
            .transpose()
    }

    pub(super) fn lower_order(
        &mut self,
        order: &OrderBy,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<OrderPlan> {
        Ok(OrderPlan {
            expr: self.lower_scalar(&order.expr, aggregates, arena)?,
            descending: order.descending,
            nulls: order.nulls,
        })
    }

    pub(super) fn lower_expression(
        &mut self,
        expression: &Expr,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<ExpressionPlan> {
        let mut arena = QueryArena::new(self.control, self.parameters);
        let scalar = self.lower_scalar(expression, aggregates, &mut arena)?;
        Ok(ExpressionPlan {
            scalar,
            subqueries: self.take_arena(arena)?,
        })
    }
}
