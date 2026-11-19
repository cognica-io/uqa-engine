//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete query transformation follows the existing CTE, source, scalar and child-query order without moving selected syntax before its callback.

use super::{
    MutableQueryPlan, QueryPlan, RelationalPlan, RewriteMemory, SQLError, SourceChildSlot,
    SourcePlan,
};
use crate::plan::{CtePlan, CtePlanBody, ScalarNodeTransform};
use input::{checkpoint, Immediate, Input};
use uqa_core::memory::ProductionControl;
pub(in crate::plan::mutable) mod input;

pub(in crate::plan::mutable) fn transform_query(
    query: &mut QueryPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(query_from(
        query,
        memory,
        control,
        &mut Immediate::new(transform),
    ))
}

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

pub(in crate::plan::mutable) async fn query_from<I: Input>(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    ctes_from(
        &mut query.ctes,
        memory
            .as_mut()
            .map(|memory| memory.field_mut("ctes").as_mut().expect("CTE vector")),
        control,
        input,
    )
    .await?;
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            for pending in &mut block.deferred_outer_rewrites {
                input.deferred(pending, control)?.await?;
            }
            if let Some(source) = &mut block.from {
                let memory = memory
                    .as_mut()
                    .map(|memory| super::super::sources::query_source_memory(memory));
                input.source(source, memory, control)?.await?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            input
                .query(
                    left,
                    memory.as_mut().map(|memory| query_child(memory, "left")),
                    control,
                )?
                .await?;
            input
                .query(
                    right,
                    memory.as_mut().map(|memory| query_child(memory, "right")),
                    control,
                )?
                .await?;
        }
        RelationalPlan::Values { .. } => {}
    }
    super::roots::query(
        query,
        memory.as_deref_mut(),
        super::roots::QueryScope::Analysis,
        control,
        input,
    )
    .await?;
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
        });
        input.query(query, memory, control)?.await?;
    }
    checkpoint(control, input).await?;
    Ok(())
}

fn query_child<'a>(memory: &'a mut RewriteMemory, field: &str) -> &'a mut Option<RewriteMemory> {
    memory
        .field_mut("root")
        .as_mut()
        .expect("query root")
        .field_mut(field)
        .as_mut()
        .expect("query child box")
        .field_mut("query")
}

pub(in crate::plan::mutable) async fn source_from<I: Input>(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    source_children_from(source, memory.as_deref_mut(), control, input).await?;
    super::roots::source(source, memory, control, input).await
}

pub(in crate::plan::mutable) async fn source_children_from<I: Input>(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    match source {
        SourcePlan::Join { left, right, .. } => {
            input
                .source(
                    left,
                    memory
                        .as_mut()
                        .map(|memory| SourceChildSlot::Left.memory_mut(memory)),
                    control,
                )?
                .await?;
            input
                .source(
                    right,
                    memory
                        .as_mut()
                        .map(|memory| SourceChildSlot::Right.memory_mut(memory)),
                    control,
                )?
                .await?;
        }
        SourcePlan::Subquery { body, .. } => {
            let memory = memory.as_mut().map(|memory| {
                memory
                    .field_mut("body")
                    .as_mut()
                    .expect("source query box")
                    .field_mut("query")
            });
            input.query(body, memory, control)?.await?;
        }
        _ => {}
    }
    Ok(())
}

pub(in crate::plan::mutable) async fn ctes_from<I: Input>(
    ctes: &mut [CtePlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, cte) in ctes.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        let mut item = memory
            .as_mut()
            .map(|memory| memory.item_mut(index).as_mut().expect("CTE item"));
        let body = item
            .as_mut()
            .map(|item| item.field_mut("body").as_mut().expect("CTE body"));
        match &mut cte.body {
            CtePlanBody::Query(query) => {
                input
                    .query(query, body.map(|body| body.field_mut("query")), control)?
                    .await?;
            }
            CtePlanBody::Command(command) => {
                input
                    .command(command, body.map(|body| body.field_mut("command")), control)?
                    .await?;
            }
        }
        if let Some(cycle) = &mut cte.cycle {
            let mut cycle_memory =
                item.map(|item| item.field_mut("cycle").as_mut().expect("CTE cycle"));
            let mark = cycle_memory.as_mut().map(|memory| {
                memory
                    .field_mut("mark_value")
                    .as_mut()
                    .expect("cycle mark")
                    .scalar_mut()
            });
            input
                .standalone_scalar(&mut cycle.mark_value, mark, control)
                .await?;
            let default = cycle_memory.as_mut().map(|memory| {
                memory
                    .field_mut("mark_default")
                    .as_mut()
                    .expect("cycle default")
                    .scalar_mut()
            });
            input
                .standalone_scalar(&mut cycle.mark_default, default, control)
                .await?;
        }
    }
    Ok(())
}
