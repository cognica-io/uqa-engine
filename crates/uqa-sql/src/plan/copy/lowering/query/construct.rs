//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enclosing queries share field order and structural ownership across ready and suspended children.

use super::super::{
    expressions::construct_optional_scalar_box, input::scalars::construct_scalars,
    rows::construct_rows, Copying, Result,
};
use super::block;
use crate::{
    ast::{Expr, SelectStmt, CTE},
    construction::{retain, Read},
    plan::{
        retirement::{construction::Prefix, Pending},
        CtePlan, OrderPlan, QueryPlan, RelationalPlan,
    },
};
use std::future::Future;

#[derive(Clone, Copy)]
pub(in crate::plan::copy::lowering) enum Source<'s> {
    Select(&'s SelectStmt),
    Values(&'s [Vec<Expr>]),
    // An implicit set-operation left side retains only its block, without enclosing CTEs or the set itself.
    Block(&'s SelectStmt),
}

pub(in crate::plan::copy::lowering) trait Input<'s, 'c>:
    block::Input<'s, 'c>
{
    fn ctes(
        &mut self,
        source: &'s [CTE],
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<Vec<CtePlan>>>> + Send;

    fn query(
        &mut self,
        source: Source<'s>,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<QueryPlan>>> + Send;
}

pub(in crate::plan::copy::lowering) async fn construct<'s, 'c, I: Input<'s, 'c>>(
    source: Source<'s>,
    copying: &mut Copying<'c>,
    input: &mut I,
    query: &mut Prefix<QueryPlan>,
) -> Result<()> {
    copying.check()?;
    input.checkpoint().await?;
    query.analyzed_grouping = copying.field("analyzed_grouping", |_| Ok(None))?;
    query.relation_privilege_subject = copying.field("relation_privilege_subject", |_| Ok(None))?;
    query.ctes = field!(copying, "ctes", |field| match source {
        Source::Select(statement) =>
            Read::new(field.control, input.ctes(&statement.with, &mut field))?
                .await?
                .take(),
        Source::Values(_) | Source::Block(_) => Vec::new(),
    });
    query.root = field!(copying, "root", |field| retain(
        field.control,
        relational(source, &mut field, input),
    )?
    .await?
    .take());
    Ok(())
}

async fn relational<'s, 'c, I: Input<'s, 'c>>(
    source: Source<'s>,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<RelationalPlan>> {
    copying.check()?;
    match source {
        Source::Values(rows) => retain(copying.control, values(rows, copying, input))?.await,
        Source::Select(statement) if statement.set_op.is_some() => {
            retain(copying.control, set(statement, copying, input))?.await
        }
        Source::Select(statement) if !statement.values.is_empty() => {
            retain(copying.control, values(&statement.values, copying, input))?.await
        }
        Source::Select(statement) | Source::Block(statement) => {
            retain(copying.control, query_block(statement, copying, input))?.await
        }
    }
}

async fn query_block<'s, 'c, I: Input<'s, 'c>>(
    source: &'s SelectStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<RelationalPlan>> {
    let block =
        copying.boxed(|_| Ok(crate::plan::mutable::construction::empty_projection_block()))?;
    let mut block = copying.retain_plan(block);
    field!(copying, "block", |field| block::construct(
        source, &mut field, input, &mut block,
    )
    .await?);
    Ok(copying.retain_plan(RelationalPlan::QueryBlock(block.take())))
}

async fn values<'s, 'c, I: Input<'s, 'c>>(
    source: &'s [Vec<Expr>],
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<RelationalPlan>> {
    let rows = Pending::new(field!(copying, "rows", |field| construct_rows(
        source,
        input,
        field.control,
        field.fields.as_mut().expect("row records"),
        field.memory.as_mut().expect("row allowance"),
    )
    .await?));
    let subqueries = Read::new(copying.control, input.finish_arena(copying))?.await?;
    Ok(copying.retain_plan(RelationalPlan::Values {
        rows: rows.take(),
        subqueries: subqueries.take(),
    }))
}

async fn child<'s, 'c, I: Input<'s, 'c>>(
    source: Source<'s>,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<Box<QueryPlan>>> {
    copying.admit_box::<QueryPlan>()?;
    let query = Read::new(copying.control, input.query(source, copying))?.await?;
    let query = copying.finish_box(query, Prefix::take)?;
    Ok(copying.retain_plan(query))
}

async fn set<'s, 'c, I: Input<'s, 'c>>(
    statement: &'s SelectStmt,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<RelationalPlan>> {
    let set = statement.set_op.as_ref().expect("set operation");
    let left = field!(copying, "left", |field| child(
        set.left
            .as_deref()
            .map_or(Source::Block(statement), Source::Select),
        &mut field,
        input,
    )
    .await?);
    let right = field!(copying, "right", |field| child(
        Source::Select(&set.right),
        &mut field,
        input,
    )
    .await?);
    let order_by = field!(copying, "order_by", |field| construct_scalars(
        &set.combined_order_by,
        &mut field,
        input,
        |order| &order.expr,
        |order, expr| OrderPlan {
            expr,
            descending: order.descending,
            nulls: order.nulls,
        },
    )
    .await?);
    let limit = field!(copying, "limit", |field| construct_optional_scalar_box(
        set.combined_limit.as_ref(),
        &mut field,
        input,
    )
    .await?);
    let offset = field!(copying, "offset", |field| construct_optional_scalar_box(
        set.combined_offset.as_ref(),
        &mut field,
        input,
    )
    .await?);
    let subqueries = Read::new(copying.control, input.finish_arena(copying))?.await?;
    Ok(copying.retain_plan(RelationalPlan::SetOp {
        kind: set.kind,
        all: set.all,
        left: left.take(),
        right: right.take(),
        order_by: order_by.take(),
        limit: limit.take(),
        with_ties: set.combined_with_ties,
        offset: offset.take(),
        subqueries: subqueries.take(),
    }))
}
