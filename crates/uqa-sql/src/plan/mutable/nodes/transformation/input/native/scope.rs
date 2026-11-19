//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A stored scalar is edited in its original query namespace after every nested query has completed.

use super::{
    ArenaInput, Context, Future, Native, Prefix, ProductionControl, QueryMetadataUpdate, QueryPlan,
    Result, RewriteMemory, ScalarEdit, ScalarQueryArena, SourcePlan, Work,
};
use crate::plan::mutable::nodes::LocalSourceInput;

#[derive(Clone, Copy)]
pub(super) enum SourcePass {
    Full,
    Children,
    Local,
}

pub(crate) trait ArenaEdit {
    fn set_arena(&mut self, width: Option<usize>);
    fn completed_query(
        &mut self,
        query: &QueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<QueryMetadataUpdate>;
}

pub(super) trait Mode<E>: Default + Send {
    const SCOPED: bool;
    fn enter(&mut self, _: &mut E, _: ScalarQueryArena, _: &Context) {
        unreachable!("scalar editor has no query arena")
    }
    fn leave(&mut self, _: &mut E) -> ScalarQueryArena {
        unreachable!("scalar editor has no query arena")
    }
    fn completed_query(
        &mut self,
        _: &mut E,
        _: &QueryPlan,
        _: &ProductionControl<'_>,
    ) -> Result<QueryMetadataUpdate> {
        unreachable!("scalar editor has no query arena")
    }
}

#[derive(Default)]
pub(super) struct Flat;
impl<E> Mode<E> for Flat {
    const SCOPED: bool = false;
}

#[derive(Default)]
pub(super) struct Scoped(Option<Prefix<ScalarQueryArena>>);
impl<E: ArenaEdit> Mode<E> for Scoped {
    const SCOPED: bool = true;
    fn enter(&mut self, edit: &mut E, arena: ScalarQueryArena, context: &Context) {
        assert!(
            self.0.is_none(),
            "nested namespaces complete before their parent"
        );
        edit.set_arena(Some(arena.queries().len()));
        self.0 = Some(context.retain(arena));
    }
    fn leave(&mut self, edit: &mut E) -> ScalarQueryArena {
        edit.set_arena(None);
        self.0.take().expect("active scalar-query namespace").take()
    }
    fn completed_query(
        &mut self,
        edit: &mut E,
        query: &QueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<QueryMetadataUpdate> {
        edit.completed_query(query, control)
    }
}

impl<'a, E, I, M> ArenaInput for Native<'a, E, I, M>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
    M: Mode<E> + 'a,
{
    fn enter_arena(&mut self, arena: ScalarQueryArena) {
        self.mode.enter(
            self.edit.as_mut().expect("active scalar editor"),
            arena,
            &self.retirement,
        );
    }
    fn leave_arena(&mut self) -> ScalarQueryArena {
        self.mode
            .leave(self.edit.as_mut().expect("active scalar editor"))
    }
    async fn complete(
        &mut self,
        query: &mut QueryPlan,
        memory: Option<&mut RewriteMemory>,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        self.mode
            .completed_query(
                self.edit.as_mut().expect("active scalar editor"),
                query,
                control,
            )?
            .install(query, memory, control)
    }
}

impl<'a, E, I, M> LocalSourceInput for Native<'a, E, I, M>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
    M: Mode<E> + 'a,
{
    fn local_source<'v>(
        &mut self,
        value: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(self.source_part(value, memory, control, SourcePass::Local))
    }
}
