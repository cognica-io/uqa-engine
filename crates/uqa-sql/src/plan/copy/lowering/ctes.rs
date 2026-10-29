//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result};
use crate::{
    ast::{CteBody, CTE},
    plan::{AggregateClassifier, CommandPlan, CteCyclePlan, CtePlan, CtePlanBody, CteSearchPlan},
};

impl Copying<'_> {
    pub(super) fn lower_cte(
        &mut self,
        cte: &CTE,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<CtePlan> {
        Ok(CtePlan {
            name: self.text(&cte.name)?,
            columns: self.texts(&cte.columns)?,
            recursive: cte.recursive,
            materialization: cte.materialization,
            search: cte
                .search
                .as_ref()
                .map(|search| -> Result<_> {
                    Ok(CteSearchPlan {
                        columns: self.texts(&search.columns)?,
                        breadth_first: search.breadth_first,
                        sequence_column: self.text(&search.sequence_column)?,
                    })
                })
                .transpose()?,
            cycle: self.field("cycle", |copying| {
                cte.cycle
                    .as_ref()
                    .map(|cycle| -> Result<_> {
                        let mut mark = QueryArena::new(copying.control, copying.parameters);
                        let mut default = QueryArena::new(copying.control, copying.parameters);
                        Ok(CteCyclePlan {
                            columns: copying.texts(&cycle.columns)?,
                            mark_column: copying.text(&cycle.mark_column)?,
                            mark_value: copying.field("mark_value", |copying| {
                                copying.lower_scalar(&cycle.mark_value, aggregates, &mut mark)
                            })?,
                            mark_default: copying.field("mark_default", |copying| {
                                copying.lower_scalar(&cycle.mark_default, aggregates, &mut default)
                            })?,
                            path_column: copying.text(&cycle.path_column)?,
                        })
                    })
                    .transpose()
            })?,
            body: self.field("body", |copying| {
                Ok(match &cte.body {
                    CteBody::Query(query) => CtePlanBody::Query(
                        copying.boxed(|copying| copying.lower_nested_query(query, aggregates))?,
                    ),
                    command => CtePlanBody::Command(copying.boxed(|copying| {
                        copying.field("command", |copying| {
                            copying.field("value0", |copying| {
                                Ok(match command {
                                    CteBody::Query(_) => unreachable!("query selected above"),
                                    CteBody::Insert(statement) => {
                                        CommandPlan::Insert(copying.boxed(|copying| {
                                            copying.lower_insert(statement, aggregates)
                                        })?)
                                    }
                                    CteBody::Update(statement) => {
                                        CommandPlan::Update(copying.boxed(|copying| {
                                            copying.lower_update(statement, aggregates)
                                        })?)
                                    }
                                    CteBody::Delete(statement) => {
                                        CommandPlan::Delete(copying.boxed(|copying| {
                                            copying.lower_delete(statement, aggregates)
                                        })?)
                                    }
                                    CteBody::Merge(statement) => {
                                        CommandPlan::Merge(copying.boxed(|copying| {
                                            copying.lower_merge(statement, aggregates)
                                        })?)
                                    }
                                })
                            })
                        })
                    })?),
                })
            })?,
        })
    }
}
