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

pub(super) mod roots;
pub(super) mod transformation;
pub(super) use transformation::transform_query;

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
    memory: Option<&mut RewriteMemory>,
    where_first: bool,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(roots::query(
        query,
        memory,
        if where_first {
            roots::QueryScope::Analysis
        } else {
            roots::QueryScope::Local
        },
        control,
        &mut roots::Immediate { action, work: () },
    ))
}

pub(super) fn scalar_memory(memory: &mut Option<ScalarMemory>) -> &mut ScalarMemory {
    memory.as_mut().expect("scalar owner")
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
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(roots::source(
        source,
        memory,
        control,
        &mut roots::Immediate { action, work: () },
    ))
}
