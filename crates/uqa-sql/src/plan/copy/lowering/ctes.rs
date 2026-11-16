//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{input::InlineScalarInput, Copying, Result};
use crate::{
    ast::{CteBody, CTE},
    plan::{retirement::Pending, AggregateClassifier, CommandPlan, CtePlan, CtePlanBody},
};

mod metadata;

#[cfg(test)]
mod tests;

impl Copying<'_> {
    pub(super) fn lower_cte(
        &mut self,
        cte: &CTE,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<CtePlan> {
        let mut scope = self.enter_plan_node()?;
        scope.lower_cte_fields(cte, aggregates)
    }

    fn lower_cte_fields(
        &mut self,
        cte: &CTE,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<CtePlan> {
        let name = self.text(&cte.name)?;
        let columns = self.texts(&cte.columns)?;
        let mut search_arena = self.query_arena();
        let search = match &cte.search {
            Some(search) => Some(crate::catalog::work::inline(metadata::construct_search(
                search,
                self,
                &mut InlineScalarInput {
                    aggregates,
                    arena: &mut search_arena,
                },
            ))?),
            None => None,
        };
        let cycle = self.field("cycle", |copying| {
            let Some(cycle) = &cte.cycle else {
                return Ok(Pending::new(None));
            };
            let mut mark = copying.query_arena();
            let mut default = copying.query_arena();
            let cycle = crate::catalog::work::inline(metadata::construct_cycle(
                cycle,
                copying,
                &mut InlineScalarInput {
                    aggregates,
                    arena: &mut mark,
                },
                &mut InlineScalarInput {
                    aggregates,
                    arena: &mut default,
                },
            ))?;
            Ok(Pending::new(Some(cycle.take())))
        })?;
        let body = self.field("body", |copying| {
            Ok(match &cte.body {
                CteBody::Query(query) => CtePlanBody::Query(
                    copying.lower_boxed(|copying| copying.lower_nested_query(query, aggregates))?,
                ),
                command => CtePlanBody::Command(copying.lower_boxed(|copying| {
                    copying.field("command", |copying| {
                        copying.field("value0", |copying| {
                            Ok(match command {
                                CteBody::Query(_) => unreachable!("query selected above"),
                                CteBody::Insert(statement) => {
                                    CommandPlan::Insert(copying.lower_boxed(|copying| {
                                        copying.lower_insert(statement, aggregates)
                                    })?)
                                }
                                CteBody::Update(statement) => {
                                    CommandPlan::Update(copying.lower_boxed(|copying| {
                                        copying.lower_update(statement, aggregates)
                                    })?)
                                }
                                CteBody::Delete(statement) => {
                                    CommandPlan::Delete(copying.lower_boxed(|copying| {
                                        copying.lower_delete(statement, aggregates)
                                    })?)
                                }
                                CteBody::Merge(statement) => {
                                    CommandPlan::Merge(copying.lower_boxed(|copying| {
                                        copying.lower_merge(statement, aggregates)
                                    })?)
                                }
                            })
                        })
                    })
                })?),
            })
        })?;
        Ok(CtePlan {
            name,
            columns,
            recursive: cte.recursive,
            materialization: cte.materialization,
            search,
            cycle: cycle.take(),
            body,
        })
    }
}
