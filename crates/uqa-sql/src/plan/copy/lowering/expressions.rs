//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    input::{InlineScalarInput, ScalarInput},
    Copying, QueryArena, Result,
};
use crate::{
    ast::Expr,
    plan::{retirement::Pending, AggregateClassifier, ExpressionPlan},
    ScalarExpr,
};

impl Copying<'_> {
    pub(super) fn lower_optional_scalar(
        &mut self,
        expression: Option<&Expr>,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Option<ScalarExpr>> {
        crate::catalog::work::inline(construct_optional_scalar(
            expression,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }

    pub(super) fn lower_optional_scalar_box(
        &mut self,
        expression: Option<&Expr>,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Option<Box<ScalarExpr>>> {
        crate::catalog::work::inline(construct_optional_scalar_box(
            expression,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }

    pub(super) fn lower_expression(
        &mut self,
        expression: &Expr,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<ExpressionPlan> {
        let mut scope = self.enter_plan_node()?;
        let copying = &mut *scope;
        let mut arena = copying.query_arena();
        let scalar = Pending::new(copying.lower_scalar(expression, aggregates, &mut arena)?);
        let subqueries = copying.take_arena(arena)?;
        Ok(ExpressionPlan {
            scalar: scalar.take(),
            subqueries,
        })
    }
}

pub(super) async fn construct_optional_scalar<'s, I: ScalarInput<&'s Expr>>(
    expression: Option<&'s Expr>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Option<ScalarExpr>>> {
    copying.check()?;
    input.checkpoint().await?;
    copying.check()?;
    let Some(expression) = expression else {
        return Ok(Pending::new(None));
    };
    let value = input.scalar(expression, copying.control).await?;
    Ok(Pending::new(Some(copying.take_scalar(value)?)))
}

pub(super) async fn construct_optional_scalar_box<'s, I: ScalarInput<&'s Expr>>(
    expression: Option<&'s Expr>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Option<Box<ScalarExpr>>>> {
    copying.check()?;
    input.checkpoint().await?;
    copying.check()?;
    let Some(expression) = expression else {
        return Ok(Pending::new(None));
    };
    copying.reserve(size_of::<ScalarExpr>())?;
    let value = input.scalar(expression, copying.control).await?;
    Ok(Pending::new(Some(Box::new(copying.take_scalar(value)?))))
}
