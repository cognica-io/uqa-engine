//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    resources::{Lowering, Result},
    source::{Items, Source},
    AggregateClassifier, FrameBound, OrderBy, QueryPlan, ScalarFrameBound, ScalarOrder,
    ScalarWindowFrame, ScalarWindowSpec, WindowSpec,
};
use crate::ast::WindowFrame;

impl Lowering<'_> {
    pub(super) fn order(
        &mut self,
        source: Source<'_, OrderBy>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<ScalarOrder> {
        let (expression, descending, nulls) = match source {
            Source::Owned(OrderBy {
                expr,
                descending,
                nulls,
            }) => (Source::Owned(expr), descending, nulls),
            Source::Borrowed(OrderBy {
                expr,
                descending,
                nulls,
            }) => (Source::Borrowed(expr), *descending, *nulls),
        };
        Ok(ScalarOrder {
            expr: self.expression(expression, aggregates, subqueries)?,
            descending,
            nulls,
        })
    }

    pub(super) fn window(
        &mut self,
        source: Source<'_, WindowSpec>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<ScalarWindowSpec> {
        self.check()?;
        let (unresolved, partition_by, order_by, frame) = match source {
            Source::Owned(WindowSpec {
                reference,
                partition_by,
                order_by,
                frame,
            }) => (
                reference.is_some(),
                Items::Owned(partition_by.into_iter()),
                Items::Owned(order_by.into_iter()),
                frame.map(Source::Owned),
            ),
            Source::Borrowed(WindowSpec {
                reference,
                partition_by,
                order_by,
                frame,
            }) => (
                reference.is_some(),
                Items::Borrowed(partition_by.iter()),
                Items::Borrowed(order_by.iter()),
                frame.as_ref().map(Source::Borrowed),
            ),
        };
        assert!(
            !unresolved,
            "named window reference must be resolved before unified-plan lowering"
        );
        Ok(ScalarWindowSpec {
            partition_by: self.map(partition_by, |this, expression| {
                this.expression(expression, aggregates, subqueries)
            })?,
            order_by: self.map(order_by, |this, order| {
                this.order(order, aggregates, subqueries)
            })?,
            frame: frame
                .map(|frame| self.frame(frame, aggregates, subqueries))
                .transpose()?,
        })
    }

    pub(super) fn frame(
        &mut self,
        source: Source<'_, WindowFrame>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<ScalarWindowFrame> {
        let (mode, start, end) = match source {
            Source::Owned(WindowFrame { mode, start, end }) => {
                (mode, Source::Owned(start), Source::Owned(end))
            }
            Source::Borrowed(WindowFrame { mode, start, end }) => {
                (*mode, Source::Borrowed(start), Source::Borrowed(end))
            }
        };
        Ok(ScalarWindowFrame {
            mode,
            start: self.bound(start, aggregates, subqueries)?,
            end: self.bound(end, aggregates, subqueries)?,
        })
    }

    pub(super) fn bound(
        &mut self,
        source: Source<'_, FrameBound>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Result<ScalarFrameBound> {
        self.check()?;
        let (preceding, expression) = match source {
            Source::Owned(FrameBound::UnboundedPreceding)
            | Source::Borrowed(FrameBound::UnboundedPreceding) => {
                return Ok(ScalarFrameBound::UnboundedPreceding)
            }
            Source::Owned(FrameBound::UnboundedFollowing)
            | Source::Borrowed(FrameBound::UnboundedFollowing) => {
                return Ok(ScalarFrameBound::UnboundedFollowing)
            }
            Source::Owned(FrameBound::CurrentRow) | Source::Borrowed(FrameBound::CurrentRow) => {
                return Ok(ScalarFrameBound::CurrentRow)
            }
            Source::Owned(FrameBound::Preceding(expression)) => (true, Source::Owned(expression)),
            Source::Borrowed(FrameBound::Preceding(expression)) => {
                (true, Source::Borrowed(expression))
            }
            Source::Owned(FrameBound::Following(expression)) => (false, Source::Owned(expression)),
            Source::Borrowed(FrameBound::Following(expression)) => {
                (false, Source::Borrowed(expression))
            }
        };
        let expression = self.child(expression, aggregates, subqueries)?;
        Ok(if preceding {
            ScalarFrameBound::Preceding(expression)
        } else {
            ScalarFrameBound::Following(expression)
        })
    }
}
