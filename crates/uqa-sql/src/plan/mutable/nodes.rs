//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-root traversal keeps each syntax node beside its constructor owner during edits.

use super::{
    memory::RewriteMemory, sources::query_source_memory, MutableQueryPlan, MutableSourcePlan,
    OwnedScalarExpr, QueryScalarSlot, RelationalPlan, SourceChildSlot, SourceScalarSlot,
};
use crate::{
    plan::{QueryPlan, SourcePlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(super) mod roots;
pub(super) mod transformation;
pub(super) use transformation::transform_query;

type Replacement<'a> = dyn FnMut(&ScalarExpr) -> Result<Option<OwnedScalarExpr>, SQLError> + 'a;
pub(super) type ScalarMemory = super::super::scalar::ScalarMemory;
pub(super) type Action<'a> = dyn FnMut(
        &mut ScalarExpr,
        Option<&mut ScalarMemory>,
        &ProductionControl<'_>,
    ) -> Result<(), SQLError>
    + 'a;

impl MutableQueryPlan {
    /// Rewrite each scalar root belonging to this query in clause order. FROM items and nested queries keep their own namespaces.
    pub fn map_local_scalars(
        mut self,
        storage: &ProductionControl<'_>,
        workspace: &ProductionControl<'_>,
        map: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(storage);
        storage.check()?;
        let mut input = roots::MapRoots {
            map,
            storage: *storage,
            work: (),
        };
        crate::catalog::work::inline(roots::query(
            &mut self.query,
            self.memory.as_mut(),
            roots::QueryScope::Local,
            workspace,
            &mut input,
        ))?;
        Ok(self)
    }

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

impl MutableSourcePlan {
    /// Rewrite the scalar roots of this range item in declaration order. Join children and subqueries retain their own scopes.
    pub fn map_local_scalars(
        mut self,
        storage: &ProductionControl<'_>,
        workspace: &ProductionControl<'_>,
        map: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(storage);
        storage.check()?;
        let mut input = roots::MapRoots {
            map,
            storage: *storage,
            work: (),
        };
        crate::catalog::work::inline(roots::source(
            &mut self.source,
            self.memory.as_mut(),
            workspace,
            &mut input,
        ))?;
        workspace.check()?;
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
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    action: &mut Action<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(local_source_from(
        source,
        memory,
        control,
        &mut roots::Immediate { action, work: () },
    ))
}

pub(super) trait LocalSourceInput: roots::Input {
    fn local_source<'v>(
        &mut self,
        source: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl std::future::Future<Output = Result<(), SQLError>> + Send, SQLError>;
}

impl LocalSourceInput for roots::Immediate<'_, '_> {
    fn local_source<'v>(
        &mut self,
        source: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl std::future::Future<Output = Result<(), SQLError>> + Send, SQLError> {
        Ok(std::future::ready(crate::catalog::work::inline(
            local_source_from(source, memory.and_then(Option::as_mut), control, self),
        )))
    }
}

pub(super) async fn local_source_from<I: LocalSourceInput>(
    source: &mut SourcePlan,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    transformation::input::checkpoint(control, input).await?;
    if let SourcePlan::Join { left, right, .. } = source {
        input
            .local_source(
                left,
                memory
                    .as_deref_mut()
                    .map(|memory| SourceChildSlot::Left.memory_mut(memory)),
                control,
            )?
            .await?;
        input
            .local_source(
                right,
                memory
                    .as_deref_mut()
                    .map(|memory| SourceChildSlot::Right.memory_mut(memory)),
                control,
            )?
            .await?;
    }
    roots::source(source, memory, control, input).await
}
