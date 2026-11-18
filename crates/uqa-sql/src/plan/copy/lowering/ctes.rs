//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTEs retain ordered metadata and independent body owners through every suspended child.

use super::{
    input::{text, InlineScalarInput, ScalarInput, TextInput},
    Copying, Result,
};
use crate::{
    ast::{CteBody, CteCycleClause, Expr, SelectStmt, CTE},
    construction::Read,
    plan::{
        retirement::{construction::Prefix, Pending},
        AggregateClassifier, CommandPlan, CteCyclePlan, CtePlan, CtePlanBody, QueryPlan,
    },
};
use std::future::Future;
use uqa_core::memory::BudgetedVec;

mod inline;
pub(super) mod metadata;

#[cfg(test)]
mod tests;

pub(super) trait Input<'s, 'c>: ScalarInput<&'s Expr> + TextInput<&'s str> {
    fn cte(
        &mut self,
        source: &'s CTE,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<CtePlan>>> + Send;
    fn cycle(
        &mut self,
        source: &'s CteCycleClause,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Pending<CteCyclePlan>>> + Send;
    fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<QueryPlan>>> + Send;
    fn command(
        &mut self,
        source: &'s CteBody,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<CommandPlan>>> + Send;
}

impl Copying<'_> {
    pub(super) fn lower_ctes(
        &mut self,
        source: &[CTE],
        aggregates: &dyn AggregateClassifier,
    ) -> Result<Vec<CtePlan>> {
        let mut arena = self.query_arena();
        crate::catalog::work::inline(construct_list(
            source,
            self,
            &mut InlineScalarInput {
                aggregates,
                arena: &mut arena,
            },
        ))
        .map(Prefix::take)
    }

    pub(super) fn lower_cte(
        &mut self,
        source: &CTE,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<CtePlan> {
        let mut scope = self.enter_plan_node()?;
        let mut arena = scope.query_arena();
        crate::catalog::work::inline(construct(
            source,
            &mut scope,
            &mut InlineScalarInput {
                aggregates,
                arena: &mut arena,
            },
        ))
        .map(Prefix::take)
    }
}

pub(super) async fn construct_list<'s, 'c, I: Input<'s, 'c>>(
    source: &'s [CTE],
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<Vec<CtePlan>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut output = copying.retain_plan(BudgetedVec::new(
        copying.control.budget().expect("CTE allowance"),
    ));
    output.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("CTE records")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let cte = Read::new(field.control, input.cte(source, &mut field))?.await?;
        copying.finish_input_field("item", field);
        output.push(cte.take()).expect("admitted CTE destination");
    }
    let (output, memory) = output.take().into_parts();
    copying.absorb(Some(memory));
    Ok(copying.retain_plan(output))
}

pub(super) async fn construct<'s, 'c, I: Input<'s, 'c>>(
    source: &'s CTE,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<CtePlan>> {
    copying.check()?;
    let name = copying.take(input.text(&source.name, copying.control).await?);
    let columns = text::texts(&source.columns, copying, input).await?;
    let search = match &source.search {
        Some(search) => Some(metadata::construct_search(search, copying, input).await?),
        None => None,
    };
    let cycle = field!(copying, "cycle", |field| match &source.cycle {
        Some(cycle) => Pending::new(Some(
            Read::new(field.control, input.cycle(cycle, &mut field))?
                .await?
                .take()
        )),
        None => Pending::new(None),
    });
    let body = field!(copying, "body", |field| construct_body(
        &source.body,
        &mut field,
        input
    )
    .await?);
    Ok(copying.retain_plan(CtePlan {
        name,
        columns,
        recursive: source.recursive,
        materialization: source.materialization,
        search,
        cycle: cycle.take(),
        body: body.take(),
    }))
}

async fn construct_body<'s, 'c, I: Input<'s, 'c>>(
    source: &'s CteBody,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<CtePlanBody>> {
    let body = match source {
        CteBody::Query(query) => {
            copying.admit_box::<QueryPlan>()?;
            let query = Read::new(copying.control, input.query(query, copying))?.await?;
            CtePlanBody::Query(copying.finish_box(query, Prefix::take)?)
        }
        command => {
            copying.admit_box::<CommandPlan>()?;
            let command = field!(copying, "command", |field| Read::new(
                field.control,
                input.command(command, &mut field)
            )?
            .await?);
            CtePlanBody::Command(copying.finish_box(command, Prefix::take)?)
        }
    };
    Ok(copying.retain_plan(body))
}
