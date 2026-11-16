//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    metadata, CommandPlan, Copying, CteBody, CteCycleClause, CteCyclePlan, CtePlan,
    InlineScalarInput, Input, Pending, Prefix, QueryPlan, Result, SelectStmt, CTE,
};
use std::future::{ready, Future};

impl<'s, 'c> Input<'s, 'c> for InlineScalarInput<'_, '_> {
    fn cte(
        &mut self,
        source: &'s CTE,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<CtePlan>>> + Send {
        ready(
            copying
                .lower_cte(source, self.aggregates)
                .map(|cte| copying.retain_plan(cte)),
        )
    }

    fn cycle(
        &mut self,
        source: &'s CteCycleClause,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Pending<CteCyclePlan>>> + Send {
        let mut mark = copying.query_arena();
        let mut default = copying.query_arena();
        ready(crate::catalog::work::inline(metadata::construct_cycle(
            source,
            copying,
            &mut InlineScalarInput {
                aggregates: self.aggregates,
                arena: &mut mark,
            },
            &mut InlineScalarInput {
                aggregates: self.aggregates,
                arena: &mut default,
            },
        )))
    }

    fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<QueryPlan>>> + Send {
        ready(
            copying
                .lower_nested_query(source, self.aggregates)
                .map(|query| copying.retain_plan(query)),
        )
    }

    fn command(
        &mut self,
        source: &'s CteBody,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<CommandPlan>>> + Send {
        ready(
            copying
                .field("value0", |copying| {
                    Ok(match source {
                        CteBody::Query(_) => unreachable!("query selected by CTE constructor"),
                        CteBody::Insert(statement) => {
                            CommandPlan::Insert(copying.lower_boxed(|copying| {
                                copying.lower_insert(statement, self.aggregates)
                            })?)
                        }
                        CteBody::Update(statement) => {
                            CommandPlan::Update(copying.lower_boxed(|copying| {
                                copying.lower_update(statement, self.aggregates)
                            })?)
                        }
                        CteBody::Delete(statement) => {
                            CommandPlan::Delete(copying.lower_boxed(|copying| {
                                copying.lower_delete(statement, self.aggregates)
                            })?)
                        }
                        CteBody::Merge(statement) => {
                            CommandPlan::Merge(copying.lower_boxed(|copying| {
                                copying.lower_merge(statement, self.aggregates)
                            })?)
                        }
                    })
                })
                .map(|command| copying.retain_plan(command)),
        )
    }
}
