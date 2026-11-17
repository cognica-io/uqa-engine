//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arena scope and child admission adapt the existing child-before-parent traversal.

use super::{
    inline, nodes, CommandPlan, OwnedScalarExpr, ProductionControl, QueryObserver, QueryPlan,
    RewriteMemory, SQLError, ScalarArenaTransform, ScalarExpr, ScalarQueryArena, SourcePlan,
    Walker,
};
use crate::plan::mutable::nodes::{roots, transformation::input::Input as StructuralInput};
use crate::semantics::view_rewrite::DeferredOuterRewrite;
use std::future::{ready, Future};

pub(super) trait Input: StructuralInput + nodes::LocalSourceInput {
    fn enter_arena(&mut self, arena: ScalarQueryArena);
    fn leave_arena(&mut self) -> ScalarQueryArena;
    fn complete(
        &mut self,
        query: &mut QueryPlan,
        memory: Option<&mut RewriteMemory>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

pub(super) struct Immediate<'a, 't, 'q> {
    transform: &'a mut ScalarArenaTransform<'t>,
    complete: &'a mut QueryObserver<'q>,
    arena: Option<ScalarQueryArena>,
    work: (),
}
impl<'a, 't, 'q> Immediate<'a, 't, 'q> {
    pub(super) fn new(
        transform: &'a mut ScalarArenaTransform<'t>,
        complete: &'a mut QueryObserver<'q>,
    ) -> Self {
        Self {
            transform,
            complete,
            arena: None,
            work: (),
        }
    }
}

impl roots::Input for Immediate<'_, '_, '_> {
    type Work = ();
    fn work(&mut self) -> &mut () {
        &mut self.work
    }
    fn scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<nodes::ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send {
        let arena = self.arena.as_mut().expect("active scalar-query namespace");
        let transform = &mut self.transform;
        ready(OwnedScalarExpr::transform_nodes_in_place(
            expression,
            memory.map(nodes::scalar_memory),
            control,
            &mut |identity, expression| transform(identity, expression, arena),
        ))
    }
}

impl StructuralInput for Immediate<'_, '_, '_> {
    fn standalone_scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<nodes::ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send {
        self.enter_arena(ScalarQueryArena {
            queries: Vec::new(),
            memory: None,
        });
        let result = inline(roots::Input::scalar(self, expression, memory, control));
        drop(self.leave_arena());
        ready(result)
    }
    fn query<'v>(
        &mut self,
        value: &'v mut QueryPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<(), SQLError>> + Send, SQLError> {
        Ok(ready(inline(
            Walker {
                control,
                input: self,
            }
            .query(value, memory.and_then(Option::as_mut)),
        )))
    }
    fn source<'v>(
        &mut self,
        value: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<(), SQLError>> + Send, SQLError> {
        Ok(ready(inline(nodes::transformation::source_children_from(
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
    ) -> Result<impl Future<Output = Result<(), SQLError>> + Send, SQLError> {
        Ok(ready(inline(
            Walker {
                control,
                input: self,
            }
            .command(value, memory.and_then(Option::as_mut)),
        )))
    }
    fn deferred(
        &mut self,
        value: &mut DeferredOuterRewrite,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<(), SQLError>> + Send, SQLError> {
        let arena = self.arena.as_mut().expect("active scalar-query namespace");
        let transform = &mut self.transform;
        Ok(ready(value.transform_scalar_nodes(
            control,
            &mut |identity, expression| transform(identity, expression, arena),
        )))
    }
}

impl Input for Immediate<'_, '_, '_> {
    fn enter_arena(&mut self, arena: ScalarQueryArena) {
        assert!(
            self.arena.is_none(),
            "nested query namespace completes before its parent"
        );
        self.arena = Some(arena);
    }
    fn leave_arena(&mut self) -> ScalarQueryArena {
        self.arena.take().expect("completed scalar-query namespace")
    }
    fn complete(
        &mut self,
        query: &mut QueryPlan,
        memory: Option<&mut RewriteMemory>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>> + Send {
        ready((self.complete)(query).and_then(|update| update.install(query, memory, control)))
    }
}

impl nodes::LocalSourceInput for Immediate<'_, '_, '_> {
    fn local_source<'v>(
        &mut self,
        source: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<(), SQLError>> + Send, SQLError> {
        Ok(ready(inline(nodes::local_source_from(
            source,
            memory.and_then(Option::as_mut),
            control,
            self,
        ))))
    }
}
