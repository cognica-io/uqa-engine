//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh retained statements share their scalar editor and original identities across independent child tasks.

use super::{command_from, query_from, source_from, Input, Result};
use crate::plan::mutable::arenas::nodes::{input::Input as ArenaInput, Walker};
use crate::plan::mutable::nodes::{local_source_from, transformation::source_children_from};
use crate::plan::{QueryMetadataUpdate, ScalarQueryArena};
use crate::{
    construction::{self, Read, Work},
    plan::{
        mutable::{
            edits::Slot,
            memory::RewriteMemory,
            nodes::{roots, ScalarMemory},
        },
        retirement::construction::{Context, Prefix},
        scalar::{ScalarEdit, ScalarEditIdentity},
        CommandPlan, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        MutableSourcePlan, OwnedScalarExpr, QueryPlan, SourcePlan,
    },
    semantics::view_rewrite::DeferredOuterRewrite,
    SQLError, ScalarExpr,
};
use std::future::{ready, Future};

mod scope;
pub(crate) use scope::ArenaEdit;
use scope::{Flat, Mode, Scoped, SourcePass};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{OwnedProductionControl, ProductionControl},
};

struct Native<'a, E, I, M> {
    mode: M,
    work: Work<'a>,
    retirement: Context,
    edit: Option<E>,
    inline_identity: Option<I>,
}
enum Task<I> {
    Query(Prefix<MutableQueryPlan>),
    Command(Prefix<MutableCommandPlan>),
    Source(Prefix<MutableSourcePlan>, Option<I>, SourcePass),
}
type TaskRead<'a, E, I> = Read<'a, Result<(Task<I>, E)>>;

fn run<'a, E, I, M>(
    mut task: Task<I>,
    owner: OwnedProductionControl,
    work: Work<'a>,
    edit: E,
) -> Result<TaskRead<'a, E, I>>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
    M: Mode<E> + 'a,
{
    let admission = owner.clone();
    Ok(Read::new(admission.control(), async move {
        let retirement = match &task {
            Task::Query(value) => value.context(),
            Task::Command(value) => value.context(),
            Task::Source(value, ..) => value.context(),
        }
        .clone();
        let inline_identity = match &mut task {
            Task::Source(_, identity, _) => identity.take(),
            _ => None,
        };
        let mut input = Native {
            mode: M::default(),
            work,
            retirement,
            edit: Some(edit),
            inline_identity,
        };
        let control = owner.control();
        match &mut task {
            Task::Query(value) => {
                let value = &mut **value;
                let query = &mut value.query;
                let memory = value.memory.as_mut();
                if M::SCOPED {
                    let mut walker = Walker {
                        control: &control,
                        input: &mut input,
                    };
                    Read::new(control, walker.query(query, memory))?.await?;
                } else {
                    Read::new(control, query_from(query, memory, &control, &mut input))?.await?;
                }
            }
            Task::Command(value) => {
                let value = &mut **value;
                let command = &mut value.command;
                let memory = value.memory.as_mut();
                if M::SCOPED {
                    let mut walker = Walker {
                        control: &control,
                        input: &mut input,
                    };
                    Read::new(control, walker.command(command, memory))?.await?;
                } else {
                    Read::new(
                        control,
                        command_from(command, memory, &control, &mut input, true),
                    )?
                    .await?;
                }
            }
            Task::Source(value, _, pass) => {
                let value = &mut **value;
                let source = &mut value.source;
                let memory = value.memory.as_mut();
                let scan = match pass {
                    SourcePass::Full => {
                        Read::new(control, source_from(source, memory, &control, &mut input))?
                    }
                    SourcePass::Children => Read::new(
                        control,
                        source_children_from(source, memory, &control, &mut input),
                    )?,
                    SourcePass::Local => Read::new(
                        control,
                        local_source_from(source, memory, &control, &mut input),
                    )?,
                };
                scan.await?;
            }
        }
        control.check()?;
        Ok((
            task,
            input.edit.expect("completed traversal owns its editor"),
        ))
    })?)
}

impl<'a, E, I, M> Native<'a, E, I, M>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
    M: Mode<E> + 'a,
{
    async fn source_part<'v>(
        &mut self,
        value: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
        pass: SourcePass,
    ) -> Result<()> {
        // JOIN ON is inline in SourcePlan; its identity must travel with the detached source.
        // Other scalar roots remain in their original boxes or vector allocations.
        let identity = match value {
            SourcePlan::Join { on: Some(on), .. } => Some(<E as ScalarEditIdentity>::identity(on)),
            _ => None,
        };
        let mut slot = Slot::<MutableSourcePlan>::new(value, memory);
        let task = Task::Source(self.retirement.retain(slot.take()), identity, pass);
        let child = self.child(task, control)?;
        let (task, edit) = child.await?;
        self.edit = Some(edit);
        let Task::Source(value, ..) = task else {
            unreachable!("source scalar traversal")
        };
        slot.restore(value.take());
        Ok(())
    }
    fn child(
        &mut self,
        task: Task<I>,
        control: &ProductionControl<'_>,
    ) -> Result<TaskRead<'a, E, I>> {
        let owner = control.capture();
        let mut work = self.work.clone();
        let edit = self
            .edit
            .take()
            .expect("parent transfers its editor to the child");
        Ok(Read::new(*control, async move {
            let output =
                work.submit(
                    move |work| async move { run::<_, _, M>(task, owner, work, edit)?.await },
                )?;
            construction::receive(output).await
        })?)
    }
}

impl<'a, E, I, M> roots::Input for Native<'a, E, I, M>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
    M: Mode<E> + 'a,
{
    type Work = Work<'a>;
    fn work(&mut self) -> &mut Self::Work {
        &mut self.work
    }
    async fn scalar(
        &mut self,
        expression: &mut ScalarExpr,
        memory: Option<&mut Option<ScalarMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        let identity = self
            .inline_identity
            .take()
            .unwrap_or_else(|| <E as ScalarEditIdentity>::identity(expression));
        let mut slot = Slot::<OwnedScalarExpr>::new(expression, memory);
        let value = slot.take();
        let mut edit = self.edit.take().expect("active traversal owns its editor");
        let owner = control.capture();
        let output = self.work.submit(move |mut work| async move {
            let value = value
                .edit_from(identity, &owner.control(), &mut work, &mut edit)
                .await?;
            Ok::<_, SQLError>((value, edit))
        })?;
        let (value, edit) = construction::receive(output).await?;
        self.edit = Some(edit);
        slot.restore(value);
        Ok(())
    }
}
impl<'a, E, I, M> Input for Native<'a, E, I, M>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
    M: Mode<E> + 'a,
{
    fn query<'v>(
        &mut self,
        value: &'v mut QueryPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        let mut slot = Slot::<MutableQueryPlan>::new(value, memory);
        let task = Task::Query(self.retirement.retain(slot.take()));
        let child = self.child(task, control)?;
        Ok(async move {
            let (task, edit) = child.await?;
            self.edit = Some(edit);
            let Task::Query(value) = task else {
                unreachable!("query scalar traversal")
            };
            slot.restore(value.take());
            Ok(())
        })
    }
    fn source<'v>(
        &mut self,
        value: &'v mut SourcePlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(self.source_part(
            value,
            memory,
            control,
            if M::SCOPED {
                SourcePass::Children
            } else {
                SourcePass::Full
            },
        ))
    }
    fn command<'v>(
        &mut self,
        value: &'v mut CommandPlan,
        memory: Option<&'v mut Option<RewriteMemory>>,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        let mut slot = Slot::<MutableCommandPlan>::new(value, memory);
        let task = Task::Command(self.retirement.retain(slot.take()));
        let child = self.child(task, control)?;
        Ok(async move {
            let (task, edit) = child.await?;
            self.edit = Some(edit);
            let Task::Command(value) = task else {
                unreachable!("command scalar traversal")
            };
            slot.restore(value.take());
            Ok(())
        })
    }
    fn deferred(
        &mut self,
        _: &mut DeferredOuterRewrite,
        _: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        // RetainedExecutablePlan has only the native lowering constructor and no mutable plan escape. Lowering creates no analyzed view substitutions.
        Ok(ready(Err(SQLError::Internal(
            "native lowering produced an analyzed view substitution".into(),
        ))))
    }
}

impl Prefix<MutableExecutablePlan> {
    async fn edit_with<E, I, M, W>(
        self,
        edit: E,
        control: &ProductionControl<'_>,
        input: &mut W,
    ) -> Result<(Self, E)>
    where
        E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send,
        I: Copy + Send,
        M: Mode<E>,
        W: crate::catalog::work::Work + Send,
    {
        control.check()?;
        input.checkpoint().await?;
        let mut driver = construction::Driver::new(*control)?;
        let context = self.context().clone();
        let task = match self.take() {
            MutableExecutablePlan::Query(value) => Task::Query(context.retain(value)),
            MutableExecutablePlan::Command(value) => Task::Command(context.retain(value)),
        };
        let owner = control.capture();
        let output = driver
            .work()
            .submit(move |work| async move { run::<_, _, M>(task, owner, work, edit)?.await })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        control.check()?;
        let (task, edit) = construction::receive(output).await?;
        let plan = match task {
            Task::Query(value) => value.map(MutableExecutablePlan::Query),
            Task::Command(value) => value.map(MutableExecutablePlan::Command),
            Task::Source(..) => unreachable!("executable binding root"),
        };
        Ok((plan, edit))
    }

    pub(in crate::plan) async fn edit_scalars_from_input<E, I>(
        self,
        edit: E,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(Self, E)>
    where
        E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send,
        I: Copy + Send,
    {
        self.edit_with::<_, _, Flat, _>(edit, control, input).await
    }

    pub(in crate::plan) async fn edit_stored_scalars_from<E, I>(
        self,
        edit: E,
        control: &ProductionControl<'_>,
        input: &mut (impl crate::catalog::work::Work + Send),
    ) -> Result<(Self, E)>
    where
        E: ArenaEdit + for<'w> ScalarEdit<Work<'w>, Identity = I> + Send,
        I: Copy + Send,
    {
        self.edit_with::<_, _, Scoped, _>(edit, control, input)
            .await
    }
}
