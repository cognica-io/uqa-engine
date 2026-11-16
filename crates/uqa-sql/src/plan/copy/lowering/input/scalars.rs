//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered scalar vectors share suspended field admission with ordinary query construction.

use super::super::{QueryArena, Result};
use super::{Copying, InlineScalarInput, ScalarInput};
use crate::{
    ast::{Expr, OrderBy},
    plan::{
        retirement::{Pending, Retire},
        AggregateClassifier, OrderPlan,
    },
    ScalarExpr,
};
use uqa_core::memory::BudgetedVec;

impl Copying<'_> {
    pub(in crate::plan::copy::lowering) fn lower_scalars(
        &mut self,
        expressions: &[Expr],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<ScalarExpr>> {
        crate::catalog::work::inline(construct_scalars(
            expressions,
            self,
            &mut InlineScalarInput { aggregates, arena },
            |expression| expression,
            |_, expression| expression,
        ))
        .map(Pending::take)
    }

    pub(in crate::plan::copy::lowering) fn lower_orders(
        &mut self,
        orders: &[OrderBy],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<OrderPlan>> {
        crate::catalog::work::inline(construct_scalars(
            orders,
            self,
            &mut InlineScalarInput { aggregates, arena },
            |order| &order.expr,
            |order, expr| OrderPlan {
                expr,
                descending: order.descending,
                nulls: order.nulls,
            },
        ))
        .map(Pending::take)
    }
}

pub(in crate::plan::copy::lowering) async fn construct_scalars<
    's,
    T: Sync,
    U: Retire + Send,
    I: ScalarInput<&'s Expr>,
>(
    source: &'s [T],
    copying: &mut Copying<'_>,
    input: &mut I,
    expression: fn(&T) -> &Expr,
    construct: fn(&T, ScalarExpr) -> U,
) -> Result<Pending<Vec<U>>> {
    copying.check()?;
    input.checkpoint().await?;
    copying.check()?;
    let budget = copying.control.budget().expect("controlled scalar fields");
    let mut values = Pending::new(BudgetedVec::new(budget));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("scalar fields")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let scalar = input.scalar(expression(source), copying.control).await?;
        let scalar = field.take_scalar(scalar)?;
        values
            .push(construct(source, scalar))
            .expect("admitted scalar field");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}
