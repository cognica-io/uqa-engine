//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete scalar traversal retains one semantic order across immediate and scheduled children.

use super::{query_from, source_from};
use crate::{
    catalog::work::inline,
    plan::{
        mutable::{
            commands::transformation::command_from,
            memory::RewriteMemory,
            nodes::{roots, scalar_memory, ScalarMemory},
        },
        CommandPlan, OwnedScalarExpr, QueryPlan, ScalarNodeTransform, SourcePlan,
    },
    semantics::view_rewrite::DeferredOuterRewrite,
    SQLError, ScalarExpr,
};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

type Result<T> = std::result::Result<T, SQLError>;

pub(in crate::plan::mutable) trait Input: roots::Input {
    fn standalone_scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        self.scalar(expression, memory, control)
    }
    fn query<'v>(
        &mut self,
        value: &'v mut QueryPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send>;
    fn source<'v>(
        &mut self,
        value: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send>;
    fn command<'v>(
        &mut self,
        value: &'v mut CommandPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send>;
    fn deferred(
        &mut self,
        value: &mut DeferredOuterRewrite,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send>;
}

pub(in crate::plan::mutable) use roots::checkpoint;

pub(in crate::plan::mutable) struct Immediate<'a, 'b> {
    transform: &'a mut ScalarNodeTransform<'b>,
    work: (),
}
impl<'a, 'b> Immediate<'a, 'b> {
    pub(in crate::plan::mutable) fn new(transform: &'a mut ScalarNodeTransform<'b>) -> Self {
        Self {
            transform,
            work: (),
        }
    }
}
impl roots::Input for Immediate<'_, '_> {
    type Work = ();
    fn work(&mut self) -> &mut () {
        &mut self.work
    }
    fn scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        ready(OwnedScalarExpr::transform_nodes_in_place(
            expression,
            memory.map(scalar_memory),
            control,
            self.transform,
        ))
    }
}
impl Input for Immediate<'_, '_> {
    fn query<'v>(
        &mut self,
        value: &'v mut QueryPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(ready(inline(query_from(
            value,
            memory.and_then(Option::as_mut),
            control,
            self,
        ))))
    }
    fn source<'v>(
        &mut self,
        value: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(ready(inline(source_from(
            value,
            memory.and_then(Option::as_mut),
            control,
            self,
        ))))
    }
    fn command<'v>(
        &mut self,
        value: &'v mut CommandPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(ready(inline(command_from(
            value,
            memory.and_then(Option::as_mut),
            control,
            self,
            true,
        ))))
    }
    fn deferred(
        &mut self,
        value: &mut DeferredOuterRewrite,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(ready(value.transform_scalar_nodes(control, self.transform)))
    }
}

pub(in crate::plan) mod native;
