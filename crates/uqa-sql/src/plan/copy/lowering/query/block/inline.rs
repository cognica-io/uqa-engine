//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Expr, FromClause, Input, Prefix, QueryBlockPlan, QueryPlan, Result};
use crate::plan::{
    copy::lowering::{input::ScalarInput, input::TextInput, QueryArena},
    scalar::lower_scalar_with_arena,
    AggregateClassifier, ComputePlan, OwnedScalarExpr, SourcePlan,
};
use std::future::{ready, Future};
use uqa_core::memory::{Produced, ProductionControl};

pub(in crate::plan::copy::lowering::query) struct Inline<'a, 'c> {
    pub(in crate::plan::copy::lowering::query) aggregates: &'a dyn AggregateClassifier,
    pub(in crate::plan::copy::lowering::query) arena: Option<QueryArena<'c>>,
}

impl<'s> ScalarInput<&'s Expr> for Inline<'_, '_> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<()>> + Send {
        ready(Ok(()))
    }

    fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<OwnedScalarExpr>> + Send {
        ready(
            lower_scalar_with_arena(
                expression,
                self.aggregates,
                self.arena.as_mut().expect("active query arena"),
                control,
            )
            .map_err(Into::into),
        )
    }
}

impl<'s> TextInput<&'s str> for Inline<'_, '_> {
    fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>>> + Send {
        ready(control.copy_text(source).map_err(Into::into))
    }
}

impl<'s, 'c> Input<'s, 'c> for Inline<'_, '_> {
    fn source(
        &mut self,
        source: &'s FromClause,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<SourcePlan>>> + Send {
        ready(
            copying
                .lower_source(
                    source,
                    self.aggregates,
                    self.arena.as_mut().expect("active query arena"),
                )
                .map(|source| copying.retain_plan(source)),
        )
    }

    fn finish_arena(
        &mut self,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<Vec<QueryPlan>>>> + Send {
        ready(
            copying
                .take_arena(self.arena.take().expect("completed query arena"))
                .map(|queries| copying.retain_plan(queries)),
        )
    }

    fn compute(
        &mut self,
        block: &QueryBlockPlan,
        control: &ProductionControl<'c>,
    ) -> impl Future<Output = Result<ComputePlan>> + Send {
        ready(block.compute_kind_with_control(self.aggregates, control))
    }
}

impl<'s, 'c> super::super::construct::Input<'s, 'c> for Inline<'_, '_> {
    fn ctes(
        &mut self,
        source: &'s [crate::ast::CTE],
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<Vec<crate::plan::CtePlan>>>> + Send {
        ready(
            copying
                .map_plans(source.iter(), |copying, cte| {
                    copying.lower_cte(cte, self.aggregates)
                })
                .map(|ctes| copying.retain_plan(ctes)),
        )
    }

    fn query(
        &mut self,
        source: super::super::construct::Source<'s>,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<QueryPlan>>> + Send {
        use super::super::construct::Source;
        ready(
            match source {
                Source::Select(statement) => copying.lower_nested_query(statement, self.aggregates),
                Source::Values(_) | Source::Block(_) => copying.field("query", |copying| {
                    copying.lower_query_source(source, self.aggregates)
                }),
            }
            .map(|query| copying.retain_plan(query)),
        )
    }
}
