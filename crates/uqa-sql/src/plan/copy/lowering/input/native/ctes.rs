//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    child, construct, construction, mutations, Arena, Copying, CtePlan, Expr, Native, Prefix,
    QueryPlan, Result, ScalarInput, SelectStmt, TextInput,
};
use crate::plan::retirement::Pending;
use crate::{
    ast::{CteBody, CteCycleClause, CTE},
    plan::{copy::lowering::ctes as constructor, CommandPlan, CteCyclePlan},
};

pub(super) fn list_input<'s: 'a, 'c: 'a, 'a, I: constructor::Input<'s, 'c> + Send + 'a>(
    source: &'s [CTE],
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<Vec<CtePlan>>>>> {
    Ok(construction::Read::new(
        copying.control,
        constructor::construct_list(source, copying, input),
    )?)
}

fn cte_input<'s: 'a, 'c: 'a, 'a, I: constructor::Input<'s, 'c> + Send + 'a>(
    source: &'s CTE,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Prefix<CtePlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let mut scope = copying.enter_plan_node()?;
        constructor::construct(source, &mut scope, input).await
    })?)
}

fn cycle_input<'s: 'a, 'c: 'a, 'a, M, D>(
    source: &'s CteCycleClause,
    copying: &'a mut Copying<'c>,
    mark: &'a mut M,
    default: &'a mut D,
) -> Result<construction::Read<'a, Result<Pending<CteCyclePlan>>>>
where
    M: ScalarInput<&'s Expr> + TextInput<&'s str> + Send + 'a,
    D: ScalarInput<&'s Expr> + Send + 'a,
{
    Ok(construction::Read::new(
        copying.control,
        constructor::metadata::construct_cycle(source, copying, mark, default),
    )?)
}

impl<'s: 'a, 'c: 'a, 'a> constructor::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn cte(&mut self, source: &'s CTE, copying: &mut Copying<'c>) -> Result<Prefix<CtePlan>> {
        cte_input(source, copying, self)?.await
    }

    async fn cycle(
        &mut self,
        source: &'s CteCycleClause,
        copying: &mut Copying<'c>,
    ) -> Result<Pending<CteCyclePlan>> {
        let mut mark = Native {
            aggregates: self.aggregates,
            queries: Some(Arena::new(copying, self.work.clone(), self.aggregates)),
            work: self.work.clone(),
        };
        let mut default = Native {
            aggregates: self.aggregates,
            queries: Some(Arena::new(copying, self.work.clone(), self.aggregates)),
            work: self.work.clone(),
        };
        let result = cycle_input(source, copying, &mut mark, &mut default)?.await;
        result
    }

    async fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<QueryPlan>> {
        child(
            construct::Source::Select(source),
            copying,
            &mut self.work,
            self.aggregates,
        )
        .await
    }

    async fn command(
        &mut self,
        source: &'s CteBody,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<CommandPlan>> {
        mutations::child(
            crate::plan::copy::lowering::mutations::Source::cte(source),
            copying,
            &mut self.work,
            self.aggregates,
        )
        .await
    }
}
