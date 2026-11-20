//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled scalar observation follows the ordinary root order. Child iterators and slice tails retain their original borrowed positions across yields.

use super::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, ScalarExpr, SourcePlan};
use crate::{
    catalog::work::Work,
    construction::Stack,
    ir::{
        children::Scope,
        observation::{visit_all, ScalarObserver},
    },
    plan::{query::ExpressionOrder, ExpressionPlan, InsertPlan, PlanRef},
    semantics::view_rewrite::DeferredOuterRewrite,
    SQLError,
};
use uqa_core::memory::ProductionControl;

enum Node<'a, D> {
    Plan(PlanRef<'a>),
    Root(&'a RelationalPlan),
    Local(&'a RelationalPlan),
    Source(&'a SourcePlan),
    SourceExpressions(&'a SourcePlan),
    Ctes(&'a [CtePlan]),
    Queries(&'a [QueryPlan]),
    Rewrites(&'a [DeferredOuterRewrite]),
    Replacements(D),
    Parameters(&'a [ExpressionPlan]),
    Command(&'a CommandPlan),
    MutationExpressions(&'a CommandPlan),
    InsertResults(&'a InsertPlan),
    Scalar(&'a ScalarExpr),
}

type Nodes<'a, D> = Stack<Node<'a, D>, 4>;

pub(crate) async fn visit_scalar_nodes_with_work<W: Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
) -> Result<(), SQLError> {
    walk(plan, control, work, visit, MutationOrder::Visit).await
}

/// Preserve the order of stored-definition diagnostics when a mutation interleaves scalar roots, WITH queries and its input relation.
pub(crate) async fn visit_stored_definition_scalars<W: Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
) -> Result<(), SQLError> {
    walk(plan, control, work, visit, MutationOrder::Definition).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MutationOrder {
    Visit,
    Definition,
}

async fn walk<W: Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
    order: MutationOrder,
) -> Result<(), SQLError> {
    let mut nodes = Nodes::new(*control);
    nodes.push(Node::Plan(plan))?;
    while let Some(node) = nodes.pop() {
        checkpoint(control, work).await?;
        match node {
            Node::Plan(PlanRef::Query(query)) => {
                nodes.push(Node::Root(&query.root))?;
                if !query.ctes.is_empty() {
                    nodes.push(Node::Ctes(&query.ctes))?;
                }
            }
            Node::Plan(PlanRef::Command(command)) => {
                nodes.push(Node::Command(command))?;
                if !command.ctes().is_empty() {
                    nodes.push(Node::Ctes(command.ctes()))?;
                }
                if order == MutationOrder::Definition {
                    if let CommandPlan::Insert(insert) = command {
                        expressions(insert.target_steps(), control, work, visit).await?;
                    }
                }
            }
            Node::Root(root) => push_root(root, &mut nodes)?,
            Node::Local(root) => local(root, control, work, visit).await?,
            Node::Source(source) => {
                if let SourcePlan::Subquery { body, .. } = source {
                    nodes.push(Node::Plan(PlanRef::Query(body)))?;
                } else {
                    let (children, _) = source.expression_inputs();
                    nodes.push(Node::SourceExpressions(source))?;
                    for child in children.into_iter().rev().flatten() {
                        nodes.push(Node::Source(child))?;
                    }
                }
            }
            Node::SourceExpressions(source) => {
                expressions(source.expression_inputs().1, control, work, visit).await?;
            }
            Node::Ctes(ctes) => {
                push_cte(ctes, &mut nodes)?;
            }
            Node::Queries(queries) => {
                if let Some((first, tail)) = queries.split_first() {
                    if !tail.is_empty() {
                        nodes.push(Node::Queries(tail))?;
                    }
                    nodes.push(Node::Plan(PlanRef::Query(first)))?;
                }
            }
            Node::Rewrites(rewrites) => {
                if let Some((rewrite, tail)) = rewrites.split_first() {
                    nodes.push(Node::Rewrites(tail))?;
                    nodes.push(Node::Replacements(rewrite.expressions()))?;
                }
            }
            Node::Replacements(mut entries) => {
                if let Some(value) = entries.next() {
                    nodes.push(Node::Replacements(entries))?;
                    push_expression(value, &mut nodes)?;
                }
            }
            Node::Parameters(params) => {
                if let Some((param, tail)) = params.split_first() {
                    nodes.push(Node::Parameters(tail))?;
                    push_expression(param, &mut nodes)?;
                }
            }
            Node::Command(command) => {
                visit_command(command, &mut nodes, control, work, visit, order).await?;
            }
            Node::MutationExpressions(command) => {
                expressions(command.expression_steps(), control, work, visit).await?;
            }
            Node::InsertResults(insert) => {
                expressions(insert.result_steps(), control, work, visit).await?;
            }
            Node::Scalar(root) => expression(root, control, work, visit).await?,
        }
    }
    Ok(())
}

async fn visit_command<'a, W: Work + Send, D: Send>(
    command: &'a CommandPlan,
    nodes: &mut Nodes<'a, D>,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
    order: MutationOrder,
) -> Result<(), SQLError> {
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            nodes.push(Node::Plan(PlanRef::Query(query)))?;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            nodes.push(Node::Plan(body.as_ref().into()))?;
        }
        CommandPlan::Call { args, .. } | CommandPlan::Execute { params: args, .. } => {
            nodes.push(Node::Parameters(args))?;
        }
        CommandPlan::Insert(insert) if order == MutationOrder::Definition => {
            if !insert.subqueries.is_empty() {
                nodes.push(Node::Queries(&insert.subqueries))?;
            }
            nodes.push(Node::InsertResults(insert))?;
            if let Some(source) = &insert.source {
                nodes.push(Node::Plan(PlanRef::Query(source)))?;
            }
            expressions(insert.row_steps(), control, work, visit).await?;
        }
        _ => {
            if order == MutationOrder::Visit {
                if let Some(source) = command.source_input() {
                    nodes.push(Node::Source(source))?;
                }
            }
            let (first, queries) = command.query_input_parts();
            if !queries.is_empty() {
                nodes.push(Node::Queries(queries))?;
            }
            if let Some(first) = first {
                nodes.push(Node::Plan(PlanRef::Query(first)))?;
            }
            if order == MutationOrder::Definition {
                nodes.push(Node::MutationExpressions(command))?;
                if let Some(source) = command.source_input() {
                    nodes.push(Node::Source(source))?;
                }
            } else {
                expressions(command.expression_steps(), control, work, visit).await?;
            }
        }
    }
    Ok(())
}

fn push_cte<'a, D>(ctes: &'a [CtePlan], nodes: &mut Nodes<'a, D>) -> Result<(), SQLError> {
    if let Some((cte, tail)) = ctes.split_first() {
        if !tail.is_empty() {
            nodes.push(Node::Ctes(tail))?;
        }
        if let Some(cycle) = &cte.cycle {
            nodes.push(Node::Scalar(&cycle.mark_default))?;
            nodes.push(Node::Scalar(&cycle.mark_value))?;
        }
        nodes.push(Node::Plan(match &cte.body {
            CtePlanBody::Query(query) => PlanRef::Query(query),
            CtePlanBody::Command(command) => PlanRef::Command(command),
        }))?;
    }
    Ok(())
}

fn push_expression<'a, D>(
    value: &'a ExpressionPlan,
    nodes: &mut Nodes<'a, D>,
) -> Result<(), SQLError> {
    if !value.subqueries.is_empty() {
        nodes.push(Node::Queries(&value.subqueries))?;
    }
    nodes.push(Node::Scalar(&value.scalar))?;
    Ok(())
}

fn push_root<'a, D>(root: &'a RelationalPlan, nodes: &mut Nodes<'a, D>) -> Result<(), SQLError> {
    let queries = match root {
        RelationalPlan::QueryBlock(block) => &block.subqueries,
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            subqueries
        }
    };
    if !queries.is_empty() {
        nodes.push(Node::Queries(queries))?;
    }
    nodes.push(Node::Local(root))?;
    match root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                nodes.push(Node::Source(source))?;
            }
            if !block.deferred_outer_rewrites.is_empty() {
                nodes.push(Node::Rewrites(&block.deferred_outer_rewrites))?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            nodes.push(Node::Plan(PlanRef::Query(right)))?;
            nodes.push(Node::Plan(PlanRef::Query(left)))?;
        }
        RelationalPlan::Values { .. } => (),
    }
    Ok(())
}

async fn local<W: Work + Send>(
    root: &RelationalPlan,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
) -> Result<(), SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            expressions(
                block.expression_steps(ExpressionOrder::Visit),
                control,
                work,
                visit,
            )
            .await
        }
        RelationalPlan::SetOp {
            order_by,
            limit,
            offset,
            ..
        } => {
            for scalar in order_by
                .iter()
                .map(|order| &order.expr)
                .chain(limit.as_deref())
                .chain(offset.as_deref())
            {
                expression(scalar, control, work, visit).await?;
            }
            Ok(())
        }
        RelationalPlan::Values { rows, .. } => {
            for row in rows {
                checkpoint(control, work).await?;
                for scalar in row {
                    expression(scalar, control, work, visit).await?;
                }
            }
            Ok(())
        }
    }
}

async fn expressions<'a, W: Work + Send>(
    items: impl Iterator<Item = Option<&'a ScalarExpr>> + Send,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
) -> Result<(), SQLError> {
    for item in items {
        if let Some(root) = item {
            expression(root, control, work, visit).await?;
        } else {
            checkpoint(control, work).await?;
        }
    }
    Ok(())
}

async fn expression<W: Work + Send>(
    root: &ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl ScalarObserver,
) -> Result<(), SQLError> {
    if root.children(Scope::Complete).len() == 0 {
        checkpoint(control, work).await?;
        return visit.scalar(root, work).await;
    }
    visit_all(root, control, work, visit).await
}

async fn checkpoint<W: Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}
