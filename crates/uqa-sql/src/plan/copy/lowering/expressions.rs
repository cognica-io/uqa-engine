//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result};
use crate::{
    ast::{Expr, FrameBound, OrderBy, Projection, WindowSpec},
    plan::{AggregateClassifier, ExpressionPlan, OrderPlan, ProjectionPlan},
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
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

    pub(super) fn lower_rows(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<Vec<ScalarExpr>>> {
        self.map(rows.iter(), |copying, row| {
            copying.map(row.iter(), |copying, expression| {
                copying.lower_scalar(expression, aggregates, arena)
            })
        })
    }

    pub(super) fn lower_projection(
        &mut self,
        projection: &Projection,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<ProjectionPlan> {
        Ok(ProjectionPlan {
            expr: self.lower_scalar(&projection.expr, aggregates, arena)?,
            alias: self.field("alias", |copying| {
                copying.optional_text(projection.alias.as_deref())
            })?,
        })
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

    pub(super) fn lower_window(
        &mut self,
        window: &WindowSpec,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<ScalarWindowSpec> {
        self.check()?;
        assert!(
            window.reference.is_none(),
            "named window reference must be resolved before unified-plan lowering"
        );
        Ok(ScalarWindowSpec {
            definition: window.definition,
            partition_by: self.field("partition_by", |copying| {
                copying.map(window.partition_by.iter(), |copying, expression| {
                    copying.lower_scalar(expression, aggregates, arena)
                })
            })?,
            order_by: self.field("order_by", |copying| {
                copying.map(window.order_by.iter(), |copying, order| {
                    Ok(ScalarOrder {
                        expr: copying.lower_scalar(&order.expr, aggregates, arena)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
            })?,
            frame: self.field("frame", |copying| {
                window
                    .frame
                    .as_ref()
                    .map(|frame| {
                        Ok(ScalarWindowFrame {
                            mode: frame.mode,
                            start: copying.field("start", |copying| {
                                copying.lower_bound(&frame.start, aggregates, arena)
                            })?,
                            end: copying.field("end", |copying| {
                                copying.lower_bound(&frame.end, aggregates, arena)
                            })?,
                            between: frame.between,
                            exclusion: frame.exclusion,
                        })
                    })
                    .transpose()
            })?,
        })
    }

    fn lower_bound(
        &mut self,
        bound: &FrameBound,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<ScalarFrameBound> {
        Ok(match bound {
            FrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
            FrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
            FrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
            FrameBound::Preceding(value) => {
                ScalarFrameBound::Preceding(self.lower_scalar_box(value, aggregates, arena)?)
            }
            FrameBound::Following(value) => {
                ScalarFrameBound::Following(self.lower_scalar_box(value, aggregates, arena)?)
            }
        })
    }
}
