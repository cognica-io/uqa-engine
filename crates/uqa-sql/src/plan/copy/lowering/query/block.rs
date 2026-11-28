//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query blocks share field ordering and retained prefixes across inline and suspended construction.

use super::super::{
    expressions::construct_optional_scalar,
    input::{scalars::construct_scalars, text, ScalarInput, TextInput},
    projections::construct_projections,
    rows::construct_rows,
    windows::construct_windows,
    Copying, Result,
};
use crate::{
    ast::{Expr, FromClause, LockingClause, SelectStmt},
    construction::{retain, Read},
    plan::{
        retirement::construction::Prefix, ComputePlan, OrderPlan, QueryBlockPlan, QueryPlan,
        SourcePlan,
    },
};
use std::future::Future;
use uqa_core::memory::{BudgetedVec, OwnedSet, ProductionControl};

mod inline;
pub(in crate::plan::copy::lowering) use inline::Inline;

pub(in crate::plan::copy::lowering) trait Input<'s, 'c>:
    ScalarInput<&'s Expr> + TextInput<&'s str>
{
    fn source(
        &mut self,
        source: &'s FromClause,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<SourcePlan>>> + Send;

    fn finish_arena(
        &mut self,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<Vec<QueryPlan>>>> + Send;

    fn compute(
        &mut self,
        block: &QueryBlockPlan,
        control: &ProductionControl<'c>,
    ) -> impl Future<Output = Result<ComputePlan>> + Send;
}

pub(in crate::plan::copy::lowering) async fn construct<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s SelectStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
    block: &mut Prefix<Box<QueryBlockPlan>>,
) -> Result<()> {
    copying.check()?;
    input.checkpoint().await?;
    // Each group borrows the retained root; its admitted future does not enlarge its parent's poll frame.
    let control = copying.control;
    retain(control, projections(statement, block, copying, input))?.await?;
    retain(control, source_and_filter(statement, block, copying, input))?.await?;
    retain(control, grouping(statement, block, copying, input))?.await?;
    retain(control, ordering(statement, block, copying, input))?.await?;
    block.subqueries = Read::new(control, input.finish_arena(copying))?
        .await?
        .take();
    block.locking = field!(copying, "locking", |field| locking(
        &statement.locking,
        &mut field,
        input,
    )
    .await?);
    let compute = Read::new(control, input.compute(block, &control))?.await?;
    block.compute = compute;
    copying.check()?;
    Ok(())
}

async fn projections<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s SelectStmt,
    block: &mut QueryBlockPlan,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<()> {
    // Scalar query slots follow projection roots, windows, FROM and the remaining query fields.
    block.projections = field!(copying, "projections", |field| construct_projections(
        &statement.projections,
        &mut field,
        input,
    )
    .await?
    .take());
    block.windows = field!(copying, "windows", |field| construct_windows(
        &statement.windows,
        &mut field,
        input,
    )
    .await?
    .take());
    block.deferred_outer_rewrites = copying.field("deferred_outer_rewrites", |copying| {
        copying.map(std::iter::empty(), |_, rewrite| Ok(rewrite))
    })?;
    block.privilege_columns = copying.field("privilege_columns", |_| Ok(OwnedSet::new()))?;
    block.privilege_subqueries = copying.field("privilege_subqueries", |_| Ok(Vec::new()))?;
    Ok(())
}

async fn source_and_filter<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s SelectStmt,
    block: &mut QueryBlockPlan,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<()> {
    block.from = field!(copying, "from", |field| match &statement.from {
        Some(source) => Some(
            Read::new(field.control, input.source(source, &mut field))?
                .await?
                .take(),
        ),
        None => None,
    });
    block.r#where = field!(copying, "where", |field| construct_optional_scalar(
        statement.r#where.as_ref(),
        &mut field,
        input,
    )
    .await?
    .take());
    Ok(())
}

async fn grouping<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s SelectStmt,
    block: &mut QueryBlockPlan,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<()> {
    block.group_by = field!(copying, "group_by", |field| construct_scalars(
        &statement.group_by,
        &mut field,
        input,
        |expression| expression,
        |_, expression| expression,
    )
    .await?
    .take());
    block.grouping_sets = field!(copying, "grouping_sets", |field| construct_rows(
        &statement.grouping_sets,
        input,
        field.control,
        field.fields.as_mut().expect("grouping set records"),
        field.memory.as_mut().expect("grouping set allowance"),
    )
    .await?);
    block.group_distinct = statement.group_distinct;
    block.having = field!(copying, "having", |field| construct_optional_scalar(
        statement.having.as_ref(),
        &mut field,
        input,
    )
    .await?
    .take());
    Ok(())
}

async fn ordering<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s SelectStmt,
    block: &mut QueryBlockPlan,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<()> {
    block.order_by = field!(copying, "order_by", |field| construct_scalars(
        &statement.order_by,
        &mut field,
        input,
        |order| &order.expr,
        |order, expr| OrderPlan {
            expr,
            descending: order.descending,
            nulls: order.nulls,
        },
    )
    .await?
    .take());
    block.limit = field!(copying, "limit", |field| construct_optional_scalar(
        statement.limit.as_ref(),
        &mut field,
        input,
    )
    .await?
    .take());
    block.with_ties = statement.with_ties;
    block.offset = field!(copying, "offset", |field| construct_optional_scalar(
        statement.offset.as_ref(),
        &mut field,
        input,
    )
    .await?
    .take());
    block.distinct = statement.distinct;
    block.distinct_on = field!(copying, "distinct_on", |field| construct_scalars(
        &statement.distinct_on,
        &mut field,
        input,
        |expression| expression,
        |_, expression| expression,
    )
    .await?
    .take());
    Ok(())
}

async fn locking<'s, 'c, I: Input<'s, 'c>>(
    source: &'s [LockingClause],
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Vec<LockingClause>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = BudgetedVec::new(copying.control.budget().expect("locking allowance"));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("locking records")
        .reserve(source.len())?;
    for clause in source {
        input.checkpoint().await?;
        let mut item = copying.begin_input_field()?;
        let strength = item.field("strength", |_| Ok(clause.strength))?;
        let wait = item.field("wait", |_| Ok(clause.wait))?;
        let relations = field!(item, "relations", |field| text::texts(
            &clause.relations,
            &mut field,
            input,
        )
        .await?);
        values
            .push(LockingClause {
                strength,
                wait,
                relations,
            })
            .expect("admitted locking slot");
        copying.finish_input_field("item", item);
    }
    let (values, memory) = values.into_parts();
    copying.absorb(Some(memory));
    Ok(values)
}
