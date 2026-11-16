//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar construction suspends inside payloads and between children without recursive polling.

use super::{Control, Expr, Lowering, OwnedScalarExpr, ScalarExpr, ScalarMemory};
use crate::{plan::scalar::owned::child_slot_mut, SQLError};
use std::future::Future;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{BudgetedVec, ProductionControl},
};

mod children;
mod nodes;

/// Query slots and parameter substitutions belong to the enclosing private plan constructor. Native implementations retain their own query construction state through the supplied work channel and selected production control; a failed enclosing construction retires its private arena.
pub trait ScalarQueryInput: Send {
    fn parameter(
        &mut self,
        _expression: &Expr,
        control: &ProductionControl<'_>,
        _input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = Result<Option<usize>, SQLError>> + Send {
        async {
            control.check()?;
            Ok(None)
        }
    }

    fn append(
        &mut self,
        query: &crate::ast::SelectStmt,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = Result<usize, SQLError>> + Send;
}

struct NoQueries;

impl ScalarQueryInput for NoQueries {
    async fn append(
        &mut self,
        _: &crate::ast::SelectStmt,
        control: &ProductionControl<'_>,
        _: &mut AsyncImageInput<'_>,
    ) -> Result<usize, SQLError> {
        control.check()?;
        Err(crate::schema::retention::CatalogRetentionError::UnexpectedSubquery.into())
    }
}

struct Frame<'s, 'c> {
    source: Option<&'s Expr>,
    // Retire partially installed nodes before returning their direct and child allowances.
    output: ScalarExpr,
    lowering: Lowering<'c>,
    next: usize,
}

impl Drop for Frame<'_, '_> {
    fn drop(&mut self) {
        crate::ir::ownership::retire(&mut self.output);
    }
}

impl<'s, 'c> Frame<'s, 'c> {
    async fn new<Q: ScalarQueryInput>(
        source: &'s Expr,
        control: ProductionControl<'c>,
        queries: &mut Q,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut lowering = Lowering {
            control: Some(Control::from_production(control)),
            owned_children: Some(BudgetedVec::new(
                control.budget().expect("native scalar allowance"),
            )),
        };
        let (source, output) = match queries.parameter(source, &control, input).await? {
            Some(number) => (None, ScalarExpr::Param(number)),
            None => (
                Some(source),
                lowering.scalar_node_from(source, queries, input).await?,
            ),
        };
        Ok(Self {
            source,
            output,
            lowering,
            next: 0,
        })
    }

    async fn finish<Q: ScalarQueryInput>(
        mut self,
        queries: &mut Q,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        // IN lowers its left operand, including that operand's queries, before appending the right query. Its slot becomes valid only at this constructor boundary.
        if let Some(Expr::InSubquery { body, .. }) = self.source {
            let slot = queries
                .append(body, &self.lowering.production(), input)
                .await?;
            let ScalarExpr::InSubquery { subquery, .. } = &mut self.output else {
                unreachable!("pending IN query");
            };
            *subquery = slot;
        }
        self.lowering.production().check()?;
        Ok(OwnedScalarExpr::from_rewrite_parts(
            std::mem::replace(&mut self.output, ScalarExpr::Star),
            Some(ScalarMemory::from_parts(
                self.lowering
                    .control
                    .take()
                    .expect("native scalar owner")
                    .memory,
                self.lowering
                    .owned_children
                    .take()
                    .expect("native scalar children"),
            )),
        ))
    }

    fn attach(&mut self, child: OwnedScalarExpr) -> Result<(), SQLError> {
        let value = OwnedScalarExpr::attach_child(&mut self.lowering, child)?;
        *child_slot_mut(&mut self.output, self.next - 1).expect("suspended scalar child") = value;
        Ok(())
    }
}

impl crate::plan::ExpressionPlan {
    /// Lower scalar-only retained syntax into independently movable children through the native work channel. Scalar descendants use admitted iterative frames; names, literal values and call metadata suspend within their own payload constructors. Query children preserve the scalar-only rejection boundary.
    pub async fn lower_scalar_from_input(
        expression: &Expr,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        Self::lower_scalar_with_query_input(expression, &mut NoQueries, control, input).await
    }

    /// Lower a retained scalar into an enclosing private query arena. Query callbacks run in ordinary lowering order, and a parameter replacement suppresses traversal of its replaced syntax. The arena keeps every completed query owner until its enclosing constructor publishes or retires it.
    pub async fn lower_scalar_with_query_input<Q: ScalarQueryInput>(
        expression: &Expr,
        queries: &mut Q,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("native scalar construction requires a destination allowance".into())
        })?;
        let mut frames = BudgetedVec::new(memory);
        frames.reserve(1)?;
        frames.push(Frame::new(expression, *control, queries, input).await?)?;
        loop {
            control.check()?;
            input.checkpoint().await?;
            control.check()?;
            let parent = frames.last_mut().expect("active scalar constructor");
            if let Some(child) = parent
                .source
                .and_then(|source| children::get(source, parent.next))
            {
                parent.next += 1;
                parent
                    .lowering
                    .owned_children
                    .as_mut()
                    .expect("native scalar children")
                    .reserve(1)?;
                frames.reserve(1)?;
                frames.push(Frame::new(child, *control, queries, input).await?)?;
            } else {
                let value = frames
                    .pop()
                    .expect("completed scalar constructor")
                    .finish(queries, input)
                    .await?;
                match frames.last_mut() {
                    Some(parent) => parent.attach(value)?,
                    None => return Ok(value),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
