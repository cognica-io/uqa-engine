//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar construction suspends inside payloads and between children without recursive polling.

use super::{Control, Expr, Lowering, OwnedScalarExpr, ScalarExpr, ScalarMemory};
use crate::plan::scalar::ScalarInputWork;
use crate::{plan::scalar::owned::child_slot_mut, SQLError};
use std::future::Future;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{BudgetedSmallVec, ProductionControl},
};

mod children;
mod inline;
pub(super) use inline::{lower_inline, lower_inline_immutable};
mod nodes;
mod work;

/// Query slots and parameter substitutions belong to the enclosing private plan constructor. Native implementations retain their own query construction state through the supplied work channel and selected production control; a failed enclosing construction retires its private arena.
pub(in crate::plan) trait ScalarQueryInput<Expression, Query, E = SQLError>
where
    E: From<uqa_core::ValueRetentionError>,
{
    fn parameter(
        &mut self,
        _expression: Expression,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Option<usize>, E>> + Send {
        async {
            control.check()?;
            Ok(None)
        }
    }

    fn append(
        &mut self,
        query: Query,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<usize, E>> + Send;
}

struct NoQueries;

impl<'s> ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt> for NoQueries {
    async fn append(
        &mut self,
        _: &'s crate::ast::SelectStmt,
        control: &ProductionControl<'_>,
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
    async fn new<
        W: ScalarInputWork,
        Q: ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt, W::Error>,
    >(
        source: &'s Expr,
        control: ProductionControl<'c>,
        queries: &mut Q,
        input: &mut W,
        retain_children: bool,
    ) -> Result<Self, W::Error> {
        control.check()?;
        let mut lowering = Lowering {
            control: Some(Control::from_production(control)),
            owned_children: retain_children.then(|| {
                ScalarMemory::new(
                    control
                        .empty_reservation()
                        .expect("native scalar allowance"),
                )
            }),
        };
        let (source, output) = match queries.parameter(source, &control).await? {
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

    async fn finish<
        E: From<uqa_core::ValueRetentionError>,
        Q: ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt, E>,
    >(
        mut self,
        queries: &mut Q,
    ) -> Result<OwnedScalarExpr, E> {
        // IN lowers its left operand, including that operand's queries, before appending the right query. Its slot becomes valid only at this constructor boundary.
        if let Some(Expr::InSubquery { body, .. }) = self.source {
            let slot = queries.append(body, &self.lowering.production()).await?;
            let ScalarExpr::InSubquery { subquery, .. } = &mut self.output else {
                unreachable!("pending IN query");
            };
            *subquery = slot;
        }
        self.lowering.production().check()?;
        let control = self.lowering.control.take().expect("native scalar owner");
        let children = self
            .lowering
            .owned_children
            .take()
            .unwrap_or_else(|| ScalarMemory::new(control.memory.budget().empty_reservation()));
        Ok(OwnedScalarExpr::from_rewrite_parts(
            std::mem::replace(&mut self.output, ScalarExpr::Star),
            Some(ScalarMemory::from_parts(control.memory, children)),
        ))
    }

    fn attach(
        &mut self,
        child: OwnedScalarExpr,
    ) -> Result<(), crate::schema::retention::CatalogRetentionError> {
        let value = if self.lowering.owned_children.is_some() {
            OwnedScalarExpr::attach_child(&mut self.lowering, child)?
        } else {
            let (value, memory) = child.finish(&self.lowering.production())?.into_parts();
            self.lowering
                .control
                .as_mut()
                .expect("immutable scalar owner")
                .memory
                .absorb(memory.expect("admitted scalar payload"));
            value
        };
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
        Self::lower_scalar_with_query_input(expression, &mut NoQueries, control, input)?.await
    }

    /// Lower syntax whose dependency walk already established that it has no subquery, retaining only a suspending caller's constructor.
    pub(crate) async fn lower_stored_scalar_with_work<W: crate::catalog::work::Work + Send>(
        expression: &Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        if !W::MAY_SUSPEND || control.budget().is_none() {
            return Ok(Self::lower_scalar_for_rewrite(expression, control)?);
        }
        Self::lower_scalar_with_query_input(
            expression,
            &mut NoQueries,
            control,
            &mut crate::catalog::work::ReferenceWork { control, work },
        )?
        .await
    }

    /// Lower a retained scalar into an enclosing private query arena. Query callbacks run in ordinary lowering order, and a parameter replacement suppresses traversal of its replaced syntax. The arena keeps every completed query owner until its enclosing constructor publishes or retires it.
    pub(in crate::plan) fn lower_scalar_with_query_input<
        's: 'a,
        'a,
        W: ScalarInputWork<Error = SQLError> + 'a,
        Q: ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt> + Send + 'a,
    >(
        expression: &'s Expr,
        queries: &'a mut Q,
        control: &'a ProductionControl<'a>,
        input: &'a mut W,
    ) -> Result<crate::construction::Read<'a, Result<OwnedScalarExpr, SQLError>>, SQLError> {
        if control.budget().is_none() {
            return Err(SQLError::Internal(
                "native scalar construction requires a destination allowance".into(),
            ));
        }
        Ok(crate::construction::Read::new(
            *control,
            construct_scalar(expression, queries, control, input, true),
        )?)
    }
}

async fn construct_scalar<'s, W, Q>(
    expression: &'s Expr,
    queries: &mut Q,
    control: &ProductionControl<'_>,
    input: &mut W,
    retain_children: bool,
) -> Result<OwnedScalarExpr, W::Error>
where
    W: ScalarInputWork,
    W::Error: From<crate::schema::retention::CatalogRetentionError>,
    Q: ScalarQueryInput<&'s Expr, &'s crate::ast::SelectStmt, W::Error>,
{
    control.check()?;
    let memory = control.budget().expect("admitted scalar construction");
    let mut frames = BudgetedSmallVec::<[Frame<'_, '_>; 4]>::new(memory);
    frames
        .reserve(1)
        .map_err(uqa_core::ValueRetentionError::from)?;
    frames
        .push(Frame::new(expression, *control, queries, input, retain_children).await?)
        .map_err(uqa_core::ValueRetentionError::from)?;
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
            if let Some(children) = &mut parent.lowering.owned_children {
                children
                    .reserve_children(1)
                    .map_err(uqa_core::ValueRetentionError::from)?;
            }
            frames
                .reserve(1)
                .map_err(uqa_core::ValueRetentionError::from)?;
            frames
                .push(Frame::new(child, *control, queries, input, retain_children).await?)
                .map_err(uqa_core::ValueRetentionError::from)?;
        } else {
            let value = frames
                .pop()
                .expect("completed scalar constructor")
                .finish(queries)
                .await?;
            match frames.last_mut() {
                Some(parent) => parent.attach(value)?,
                None => return Ok(value),
            }
        }
    }
}

#[cfg(test)]
mod tests;
