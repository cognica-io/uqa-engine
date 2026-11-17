//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar field selection is shared by immediate rewrites and scheduled consumers.

use super::{QueryScalarSlot, RelationalPlan, RewriteMemory, ScalarMemory, SourceScalarSlot};
use crate::{
    catalog::work::Work,
    plan::{QueryPlan, SourcePlan},
    SQLError, ScalarExpr,
};
use std::future::Future;
use uqa_core::memory::ProductionControl;

#[derive(Clone, Copy)]
pub(in crate::plan::mutable) enum QueryScope {
    Analysis,
    Local,
    WindowUses,
}

impl QueryScope {
    fn where_first(self) -> bool {
        matches!(self, Self::Analysis)
    }
    fn include_windows(self) -> bool {
        !matches!(self, Self::WindowUses)
    }
}

pub(in crate::plan::mutable) trait Input {
    type Work: Work + Send;
    fn work(&mut self) -> &mut Self::Work;
    fn scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

pub(super) struct Immediate<'a, 'b> {
    pub(super) action: &'a mut super::Action<'b>,
    pub(super) work: (),
}

impl Input for Immediate<'_, '_> {
    type Work = ();
    fn work(&mut self) -> &mut () {
        &mut self.work
    }
    fn scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send {
        std::future::ready((self.action)(
            expression,
            memory.map(super::scalar_memory),
            control,
        ))
    }
}

async fn checkpoint<I: Input>(
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    control.check()?;
    input.work().checkpoint().await?;
    control.check()?;
    Ok(())
}

pub(in crate::plan::mutable) async fn query<I: Input>(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    scope: QueryScope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    match &query.root {
        RelationalPlan::QueryBlock(_) => {
            query_block_scalars(query, memory, scope, control, input).await?;
        }
        RelationalPlan::SetOp { order_by, .. } => {
            let count = order_by.len();
            query_slots(
                query,
                memory.as_deref_mut(),
                (0..count).map(QueryScalarSlot::OrderBy),
                control,
                input,
            )
            .await?;
            query_slots(
                query,
                memory,
                [QueryScalarSlot::Limit, QueryScalarSlot::Offset],
                control,
                input,
            )
            .await?;
        }
        RelationalPlan::Values { rows, .. } => {
            for row in 0..rows.len() {
                checkpoint(control, input).await?;
                let RelationalPlan::Values { rows, .. } = &query.root else {
                    unreachable!()
                };
                let count = rows[row].len();
                query_slots(
                    query,
                    memory.as_deref_mut(),
                    (0..count).map(|column| QueryScalarSlot::Value { row, column }),
                    control,
                    input,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn query_block_scalars<I: Input>(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    scope: QueryScope,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!("query block scalar scope");
    };
    let (projections, groups, sets, orders, distinct, windows) = (
        block.projections.len(),
        block.group_by.len(),
        block.grouping_sets.len(),
        block.order_by.len(),
        block.distinct_on.len(),
        block.windows.len(),
    );
    if scope.where_first() {
        query_slot(
            query,
            memory.as_deref_mut(),
            QueryScalarSlot::Where,
            control,
            input,
        )
        .await?;
    }
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..projections).map(QueryScalarSlot::Projection),
        control,
        input,
    )
    .await?;
    if !scope.where_first() {
        query_slot(
            query,
            memory.as_deref_mut(),
            QueryScalarSlot::Where,
            control,
            input,
        )
        .await?;
    }
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..groups).map(QueryScalarSlot::GroupBy),
        control,
        input,
    )
    .await?;
    grouping_sets(query, memory.as_deref_mut(), sets, control, input).await?;
    query_slot(
        query,
        memory.as_deref_mut(),
        QueryScalarSlot::Having,
        control,
        input,
    )
    .await?;
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..orders).map(QueryScalarSlot::OrderBy),
        control,
        input,
    )
    .await?;
    query_slots(
        query,
        memory.as_deref_mut(),
        [QueryScalarSlot::Limit, QueryScalarSlot::Offset],
        control,
        input,
    )
    .await?;
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..distinct).map(QueryScalarSlot::DistinctOn),
        control,
        input,
    )
    .await?;
    if scope.include_windows() {
        for window in 0..windows {
            query_window_scalars(query, memory.as_deref_mut(), window, control, input).await?;
        }
    }
    Ok(())
}

async fn grouping_sets<I: Input>(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    sets: usize,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for set in 0..sets {
        checkpoint(control, input).await?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!();
        };
        let count = block.grouping_sets[set].len();
        query_slots(
            query,
            memory.as_deref_mut(),
            (0..count).map(|expression| QueryScalarSlot::GroupingSet { set, expression }),
            control,
            input,
        )
        .await?;
    }
    Ok(())
}

async fn query_window_scalars<I: Input>(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    window: usize,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!("query window scalar scope");
    };
    let (partitions, orders) = (
        block.windows[window].spec.partition_by.len(),
        block.windows[window].spec.order_by.len(),
    );
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..partitions).map(|expression| QueryScalarSlot::WindowPartition { window, expression }),
        control,
        input,
    )
    .await?;
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..orders).map(|expression| QueryScalarSlot::WindowOrder { window, expression }),
        control,
        input,
    )
    .await?;
    query_slots(
        query,
        memory,
        [
            QueryScalarSlot::WindowFrameStart(window),
            QueryScalarSlot::WindowFrameEnd(window),
        ],
        control,
        input,
    )
    .await
}

async fn query_slots<I: Input>(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    slots: impl IntoIterator<Item = QueryScalarSlot>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for slot in slots {
        query_slot(query, memory.as_deref_mut(), slot, control, input).await?;
    }
    Ok(())
}

async fn query_slot<I: Input>(
    query: &mut QueryPlan,
    memory: Option<&mut RewriteMemory>,
    slot: QueryScalarSlot,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    let Some(expression) = slot.expression_mut(query) else {
        return Ok(());
    };
    let memory = memory.map(|memory| slot.memory_mut(memory));
    input.scalar(expression, memory, control).await
}

pub(in crate::plan::mutable) async fn source<I: Input>(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join { .. } => {
            source_slot(source, memory, SourceScalarSlot::JoinOn, control, input).await?;
        }
        SourcePlan::Values { rows, .. } => {
            for row in 0..rows.len() {
                checkpoint(control, input).await?;
                let SourcePlan::Values { rows, .. } = source else {
                    unreachable!()
                };
                for column in 0..rows[row].len() {
                    source_slot(
                        source,
                        memory.as_deref_mut(),
                        SourceScalarSlot::Value { row, column },
                        control,
                        input,
                    )
                    .await?;
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for index in 0..args.len() {
                source_slot(
                    source,
                    memory.as_deref_mut(),
                    SourceScalarSlot::Argument(index),
                    control,
                    input,
                )
                .await?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in 0..functions.len() {
                checkpoint(control, input).await?;
                let SourcePlan::FunctionGroup { functions, .. } = source else {
                    unreachable!()
                };
                for argument in 0..functions[function].args.len() {
                    source_slot(
                        source,
                        memory.as_deref_mut(),
                        SourceScalarSlot::GroupArgument { function, argument },
                        control,
                        input,
                    )
                    .await?;
                }
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
    Ok(())
}

async fn source_slot<I: Input>(
    source: &mut SourcePlan,
    memory: Option<&mut RewriteMemory>,
    slot: SourceScalarSlot,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    let Some(expression) = slot.expression_mut(source) else {
        return Ok(());
    };
    let memory = memory.map(|memory| slot.memory_mut(memory));
    input.scalar(expression, memory, control).await
}

#[cfg(test)]
mod tests;
