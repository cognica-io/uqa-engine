//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-root traversal keeps each syntax node beside its constructor owner during edits.

use super::{
    memory::RewriteMemory, sources::query_source_memory, MutableQueryPlan, OwnedScalarExpr,
    QueryScalarSlot, RelationalPlan, SourceChildSlot, SourceScalarSlot,
};
use crate::{
    plan::{QueryPlan, SourcePlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(super) mod transformation;
pub(super) use transformation::{transform_ctes, transform_query, transform_source};

pub(in crate::plan::mutable) fn transform_local_source(
    source: &mut SourcePlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut crate::plan::ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    local_source(
        source,
        memory,
        control,
        &mut |expression, memory, control| {
            OwnedScalarExpr::transform_nodes_in_place(expression, memory, control, transform)
        },
    )
}

type Replacement<'a> = dyn FnMut(&ScalarExpr) -> Result<Option<OwnedScalarExpr>, SQLError> + 'a;
pub(super) type ScalarMemory = super::super::scalar::ScalarMemory;
pub(super) type Action<'a> = dyn FnMut(
        &mut ScalarExpr,
        Option<&mut ScalarMemory>,
        &ProductionControl<'_>,
    ) -> Result<(), SQLError>
    + 'a;

impl MutableQueryPlan {
    /// Rewrite local scalar nodes in source-first order without moving their original addresses. The observer receives read-only syntax and returns an independently owned replacement; nested queries retain separate traversal boundaries.
    pub fn rewrite_root_scalar_nodes(
        mut self,
        control: &ProductionControl<'_>,
        replacement: &mut Replacement<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut action = |expression: &mut ScalarExpr,
                          memory: Option<&mut ScalarMemory>,
                          control: &ProductionControl<'_>| {
            OwnedScalarExpr::replace_nodes_in_place(expression, memory, control, replacement)
        };
        if let RelationalPlan::QueryBlock(block) = &mut self.query.root {
            if let Some(source) = &mut block.from {
                let memory = self
                    .memory
                    .as_mut()
                    .map(|memory| query_source_memory(memory).as_mut().expect("source owner"));
                local_source(source, memory, control, &mut action)?;
            }
        }
        query_scalars(
            &mut self.query,
            self.memory.as_mut(),
            false,
            control,
            &mut action,
        )?;
        Ok(self)
    }
}

pub(super) fn query_scalars(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    where_first: bool,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    match &query.root {
        RelationalPlan::QueryBlock(_) => {
            query_block_scalars(query, memory, where_first, control, action)?;
        }
        RelationalPlan::SetOp { order_by, .. } => {
            let count = order_by.len();
            query_slots(
                query,
                memory.as_deref_mut(),
                (0..count).map(QueryScalarSlot::OrderBy),
                control,
                action,
            )?;
            query_slots(
                query,
                memory,
                [QueryScalarSlot::Limit, QueryScalarSlot::Offset],
                control,
                action,
            )?;
        }
        RelationalPlan::Values { rows, .. } => {
            for row in 0..rows.len() {
                let RelationalPlan::Values { rows, .. } = &query.root else {
                    unreachable!()
                };
                let count = rows[row].len();
                query_slots(
                    query,
                    memory.as_deref_mut(),
                    (0..count).map(|column| QueryScalarSlot::Value { row, column }),
                    control,
                    action,
                )?;
            }
        }
    }
    Ok(())
}

fn query_block_scalars(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    where_first: bool,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
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
    if where_first {
        query_slot(
            query,
            memory.as_deref_mut(),
            QueryScalarSlot::Where,
            control,
            action,
        )?;
    }
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..projections).map(QueryScalarSlot::Projection),
        control,
        action,
    )?;
    if !where_first {
        query_slot(
            query,
            memory.as_deref_mut(),
            QueryScalarSlot::Where,
            control,
            action,
        )?;
    }
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..groups).map(QueryScalarSlot::GroupBy),
        control,
        action,
    )?;
    for set in 0..sets {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!();
        };
        let count = block.grouping_sets[set].len();
        query_slots(
            query,
            memory.as_deref_mut(),
            (0..count).map(|expression| QueryScalarSlot::GroupingSet { set, expression }),
            control,
            action,
        )?;
    }
    query_slot(
        query,
        memory.as_deref_mut(),
        QueryScalarSlot::Having,
        control,
        action,
    )?;
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..orders).map(QueryScalarSlot::OrderBy),
        control,
        action,
    )?;
    query_slots(
        query,
        memory.as_deref_mut(),
        [QueryScalarSlot::Limit, QueryScalarSlot::Offset],
        control,
        action,
    )?;
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..distinct).map(QueryScalarSlot::DistinctOn),
        control,
        action,
    )?;
    for window in 0..windows {
        query_window_scalars(query, memory.as_deref_mut(), window, control, action)?;
    }
    Ok(())
}

fn query_window_scalars(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    window: usize,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
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
        action,
    )?;
    query_slots(
        query,
        memory.as_deref_mut(),
        (0..orders).map(|expression| QueryScalarSlot::WindowOrder { window, expression }),
        control,
        action,
    )?;
    query_slots(
        query,
        memory,
        [
            QueryScalarSlot::WindowFrameStart(window),
            QueryScalarSlot::WindowFrameEnd(window),
        ],
        control,
        action,
    )
}

fn query_slots(
    query: &mut QueryPlan,
    mut memory: Option<&mut RewriteMemory>,
    slots: impl IntoIterator<Item = QueryScalarSlot>,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    for slot in slots {
        query_slot(query, memory.as_deref_mut(), slot, control, action)?;
    }
    Ok(())
}

fn query_slot(
    query: &mut QueryPlan,
    memory: Option<&mut RewriteMemory>,
    slot: QueryScalarSlot,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    let Some(expression) = slot.expression_mut(query) else {
        return Ok(());
    };
    let memory = memory.map(|memory| scalar_memory(slot.memory_mut(memory)));
    action(expression, memory, control)
}

pub(super) fn scalar_memory(memory: &mut Option<RewriteMemory>) -> &mut ScalarMemory {
    match memory.as_mut().expect("scalar owner") {
        RewriteMemory::Scalar(memory) => memory,
        RewriteMemory::Fields { .. } => unreachable!("scalar root constructor"),
    }
}

pub(super) fn local_source(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let SourcePlan::Join { left, right, .. } = source {
        local_source(
            left,
            memory.as_mut().map(|memory| {
                SourceChildSlot::Left
                    .memory_mut(memory)
                    .as_mut()
                    .expect("left source")
            }),
            control,
            action,
        )?;
        local_source(
            right,
            memory.as_mut().map(|memory| {
                SourceChildSlot::Right
                    .memory_mut(memory)
                    .as_mut()
                    .expect("right source")
            }),
            control,
            action,
        )?;
    }
    source_scalars(source, memory, control, action)
}

pub(super) fn source_scalars(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join { .. } => {
            source_slot(source, memory, SourceScalarSlot::JoinOn, control, action)?;
        }
        SourcePlan::Values { rows, .. } => {
            for row in 0..rows.len() {
                let SourcePlan::Values { rows, .. } = source else {
                    unreachable!()
                };
                for column in 0..rows[row].len() {
                    source_slot(
                        source,
                        memory.as_deref_mut(),
                        SourceScalarSlot::Value { row, column },
                        control,
                        action,
                    )?;
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
                    action,
                )?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in 0..functions.len() {
                let SourcePlan::FunctionGroup { functions, .. } = source else {
                    unreachable!()
                };
                for argument in 0..functions[function].args.len() {
                    source_slot(
                        source,
                        memory.as_deref_mut(),
                        SourceScalarSlot::GroupArgument { function, argument },
                        control,
                        action,
                    )?;
                }
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
    Ok(())
}

fn source_slot(
    source: &mut SourcePlan,
    memory: Option<&mut RewriteMemory>,
    slot: SourceScalarSlot,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    let Some(expression) = slot.expression_mut(source) else {
        return Ok(());
    };
    let memory = memory.map(|memory| scalar_memory(slot.memory_mut(memory)));
    action(expression, memory, control)
}
