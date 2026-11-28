//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compaction borrows local expression scopes without retaining child-query namespaces or recursive poll frames.

use super::{observation, Decision, Remapping, SQLError, ScalarExpr, Work};
use crate::{
    construction::Stack,
    plan::{query::ExpressionOrder, QueryPlan, RelationalPlan, SourcePlan},
};
use std::future::Future;
use uqa_core::memory::ProductionControl;

trait Visitor<S>: Send {
    fn expression<W: Work + Send>(
        &mut self,
        expression: S,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

struct Collect<'a, 'c>(&'a mut Remapping<'c>);
impl Collect<'_, '_> {
    fn slot(&mut self, id: &usize) -> Result<(), SQLError> {
        self.0.observe_slot(*id)
    }
}

impl Visitor<&ScalarExpr> for Collect<'_, '_> {
    async fn expression<W: Work + Send>(
        &mut self,
        expression: &ScalarExpr,
        _: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.0.expression_from(expression, work).await
    }
}

struct Rewrite<'a, 'c>(&'a Remapping<'c>);
impl Rewrite<'_, '_> {
    fn slot(&mut self, id: &mut usize) -> Result<(), SQLError> {
        self.0.rewrite_slot(id);
        Ok(())
    }
}

impl Visitor<&mut ScalarExpr> for Rewrite<'_, '_> {
    async fn expression<W: Work + Send>(
        &mut self,
        expression: &mut ScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        observation::visit_mut(expression, control, work, &mut |expression| {
            self.0.rewrite(expression);
            Ok(Decision::Descend)
        })
        .await?;
        Ok(())
    }
}

enum SourceFrame<N, I> {
    Node(N),
    Expressions(I),
}

async fn source<N, S, I, V, W>(
    source: N,
    split: impl Fn(N) -> ([Option<N>; 2], I),
    visitor: &mut V,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError>
where
    I: Iterator<Item = Option<S>>,
    V: Visitor<S>,
    W: Work + Send,
{
    let mut frames = Stack::<_, 4>::new(*control);
    frames.push(SourceFrame::Node(source))?;
    while let Some(frame) = frames.pop() {
        checkpoint(control, work).await?;
        match frame {
            SourceFrame::Node(source) => {
                let (children, expressions) = split(source);
                frames.push(SourceFrame::Expressions(expressions))?;
                for child in children.into_iter().rev().flatten() {
                    frames.push(SourceFrame::Node(child))?;
                }
            }
            SourceFrame::Expressions(mut expressions) => {
                if let Some(expression) = expressions.next() {
                    frames.push(SourceFrame::Expressions(expressions))?;
                    if let Some(expression) = expression {
                        visitor.expression(expression, control, work).await?;
                    }
                }
            }
        }
    }
    Ok(())
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

macro_rules! query {
    ($name:ident, $visitor:ident, $inputs:ident, $roots:ident, $iter:ident, $reference:ident, $deref:ident, $order:ident, [$($mutable:tt)*], [$($mapmutable:tt)*]) => {
        pub(in crate::plan) async fn $name<W: Work + Send>(
            query: & $($mutable)* QueryPlan,
            remap: & $($mapmutable)* Remapping<'_>,
            control: &ProductionControl<'_>,
            work: &mut W,
        ) -> Result<(), SQLError> {
            let mut visitor = $visitor(remap);
            match & $($mutable)* query.root {
                RelationalPlan::QueryBlock(block) => {
                    for slot in block.privilege_subqueries.$iter() {
                        checkpoint(control, work).await?;
                        visitor.slot(slot)?;
                    }
                    if let Some(input) = block.from.$reference() {
                        source(input, SourcePlan::$inputs, &mut visitor, control, work).await?;
                    }
                    for expression in block.$roots(ExpressionOrder::$order) {
                        checkpoint(control, work).await?;
                        if let Some(expression) = expression {
                            visitor.expression(expression, control, work).await?;
                        }
                    }
                }
                RelationalPlan::Values { rows, .. } => {
                    for row in rows.$iter() {
                        checkpoint(control, work).await?;
                        for expression in row.$iter() {
                            visitor.expression(expression, control, work).await?;
                        }
                    }
                }
                RelationalPlan::SetOp { order_by, limit, offset, .. } => {
                    for expression in order_by.$iter().map(|order| & $($mutable)* order.expr)
                        .chain(limit.$deref()).chain(offset.$deref()) {
                        visitor.expression(expression, control, work).await?;
                    }
                }
            }
            Ok(())
        }
    };
}

query!(collect, Collect, expression_inputs, expression_steps, iter, as_ref, as_deref, Initialization, [], [mut]);
query!(rewrite, Rewrite, expression_inputs_mut, expression_steps_mut, iter_mut, as_mut, as_deref_mut, Rewrite, [mut], []);
