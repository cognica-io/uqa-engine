//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete query transformation follows the existing CTE, source, scalar and child-query order without moving selected syntax before its callback.

use super::{
    query_scalars, scalar_memory, source_scalars, MutableQueryPlan, OwnedScalarExpr, QueryPlan,
    RelationalPlan, RewriteMemory, SQLError, ScalarMemory, SourceChildSlot, SourcePlan,
};
use crate::plan::{CtePlan, CtePlanBody, ScalarNodeTransform};
use uqa_core::memory::ProductionControl;

impl MutableQueryPlan {
    pub fn transform_scalar_nodes(
        mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        transform_query(&mut self.query, self.memory.as_mut(), control, transform)?;
        Ok(self)
    }
}

pub(in crate::plan::mutable) fn transform_query(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    transform_ctes(
        &mut query.ctes,
        memory
            .as_mut()
            .map(|memory| memory.field_mut("ctes").as_mut().expect("CTE vector")),
        control,
        transform,
    )?;
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            for pending in &mut block.deferred_outer_rewrites {
                pending.transform_scalar_nodes(control, transform)?;
            }
            if let Some(source) = &mut block.from {
                let memory = memory.as_mut().map(|memory| {
                    super::super::sources::query_source_memory(memory)
                        .as_mut()
                        .expect("source owner")
                });
                transform_source(source, memory, control, transform)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            transform_query(
                left,
                memory.as_mut().map(|memory| query_child(memory, "left")),
                control,
                transform,
            )?;
            transform_query(
                right,
                memory.as_mut().map(|memory| query_child(memory, "right")),
                control,
                transform,
            )?;
        }
        RelationalPlan::Values { .. } => {}
    }
    let mut action = |expression: &mut crate::ScalarExpr,
                      memory: Option<&mut ScalarMemory>,
                      control: &ProductionControl<'_>| {
        OwnedScalarExpr::transform_nodes_in_place(expression, memory, control, transform)
    };
    query_scalars(query, memory.as_deref_mut(), true, control, &mut action)?;
    let (queries, block) = match &mut query.root {
        RelationalPlan::QueryBlock(block) => (&mut block.subqueries, true),
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            (subqueries, false)
        }
    };
    let mut arena = memory.map(|memory| {
        let root = memory.field_mut("root").as_mut().expect("query root");
        let root = if block {
            root.field_mut("block").as_mut().expect("query block")
        } else {
            root
        };
        root.field_mut("subqueries")
            .as_mut()
            .expect("scalar-query arena")
    });
    for (index, query) in queries.iter_mut().enumerate() {
        let memory = arena.as_mut().map(|arena| {
            arena
                .item_mut(index)
                .as_mut()
                .expect("query item")
                .field_mut("query")
                .as_mut()
                .expect("query child")
        });
        transform_query(query, memory, control, transform)?;
    }
    control.check()?;
    Ok(())
}

fn query_child<'a>(memory: &'a mut RewriteMemory, field: &str) -> &'a mut RewriteMemory {
    memory
        .field_mut("root")
        .as_mut()
        .expect("query root")
        .field_mut(field)
        .as_mut()
        .expect("query child box")
        .field_mut("query")
        .as_mut()
        .expect("query child")
}

pub(in crate::plan::mutable) fn transform_source(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Join { left, right, .. } => {
            transform_source(
                left,
                memory.as_mut().map(|memory| {
                    SourceChildSlot::Left
                        .memory_mut(memory)
                        .as_mut()
                        .expect("left source")
                }),
                control,
                transform,
            )?;
            transform_source(
                right,
                memory.as_mut().map(|memory| {
                    SourceChildSlot::Right
                        .memory_mut(memory)
                        .as_mut()
                        .expect("right source")
                }),
                control,
                transform,
            )?;
        }
        SourcePlan::Subquery { body, .. } => {
            let memory = memory.as_mut().map(|memory| {
                memory
                    .field_mut("body")
                    .as_mut()
                    .expect("source query box")
                    .field_mut("query")
                    .as_mut()
                    .expect("source query")
            });
            transform_query(body, memory, control, transform)?;
        }
        _ => {}
    }
    source_scalars(
        source,
        memory,
        control,
        &mut |expression, memory, control| {
            OwnedScalarExpr::transform_nodes_in_place(expression, memory, control, transform)
        },
    )
}

pub(in crate::plan::mutable) fn transform_ctes(
    ctes: &mut [CtePlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, cte) in ctes.iter_mut().enumerate() {
        control.check()?;
        let mut item = memory
            .as_mut()
            .map(|memory| memory.item_mut(index).as_mut().expect("CTE item"));
        let body = item
            .as_mut()
            .map(|item| item.field_mut("body").as_mut().expect("CTE body"));
        match &mut cte.body {
            CtePlanBody::Query(query) => transform_query(
                query,
                body.map(|body| body.field_mut("query").as_mut().expect("CTE query")),
                control,
                transform,
            )?,
            CtePlanBody::Command(command) => {
                super::super::commands::transformation::transform_command(
                    command,
                    body.map(|body| body.field_mut("command").as_mut().expect("CTE command")),
                    control,
                    transform,
                )?;
            }
        }
        if let Some(cycle) = &mut cte.cycle {
            let mut cycle_memory =
                item.map(|item| item.field_mut("cycle").as_mut().expect("CTE cycle"));
            let mark = cycle_memory.as_mut().map(|memory| {
                scalar_memory(
                    memory
                        .field_mut("mark_value")
                        .as_mut()
                        .expect("cycle mark")
                        .scalar_mut(),
                )
            });
            OwnedScalarExpr::transform_nodes_in_place(
                &mut cycle.mark_value,
                mark,
                control,
                transform,
            )?;
            let default = cycle_memory.as_mut().map(|memory| {
                scalar_memory(
                    memory
                        .field_mut("mark_default")
                        .as_mut()
                        .expect("cycle default")
                        .scalar_mut(),
                )
            });
            OwnedScalarExpr::transform_nodes_in_place(
                &mut cycle.mark_default,
                default,
                control,
                transform,
            )?;
        }
    }
    Ok(())
}
