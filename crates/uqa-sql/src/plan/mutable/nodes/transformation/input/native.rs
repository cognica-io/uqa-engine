//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh retained statements share their scalar editor and original identities across independent child tasks.

use super::{command_from, query_from, source_from, Input, Result};
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
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{OwnedProductionControl, ProductionControl},
};

struct Native<'a, E, I> {
    work: Work<'a>,
    retirement: Context,
    edit: Option<E>,
    inline_identity: Option<I>,
}
enum Task<I> {
    Query(Prefix<MutableQueryPlan>),
    Command(Prefix<MutableCommandPlan>),
    Source(Prefix<MutableSourcePlan>, Option<I>),
}
type TaskRead<'a, E, I> = Read<'a, Result<(Task<I>, E)>>;

fn run<'a, E, I>(
    mut task: Task<I>,
    owner: OwnedProductionControl,
    work: Work<'a>,
    edit: E,
) -> Result<TaskRead<'a, E, I>>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
{
    let admission = owner.clone();
    Ok(Read::new(admission.control(), async move {
        let retirement = match &task {
            Task::Query(value) => value.context(),
            Task::Command(value) => value.context(),
            Task::Source(value, _) => value.context(),
        }
        .clone();
        let inline_identity = match &mut task {
            Task::Source(_, identity) => identity.take(),
            _ => None,
        };
        let mut input = Native {
            work,
            retirement,
            edit: Some(edit),
            inline_identity,
        };
        let control = owner.control();
        match &mut task {
            Task::Query(value) => {
                let value = &mut **value;
                Read::new(
                    control,
                    query_from(
                        &mut value.query,
                        value.memory.as_mut(),
                        &control,
                        &mut input,
                    ),
                )?
                .await?;
            }
            Task::Command(value) => {
                let value = &mut **value;
                Read::new(
                    control,
                    command_from(
                        &mut value.command,
                        value.memory.as_mut(),
                        &control,
                        &mut input,
                        true,
                    ),
                )?
                .await?;
            }
            Task::Source(value, _) => {
                let value = &mut **value;
                Read::new(
                    control,
                    source_from(
                        &mut value.source,
                        value.memory.as_mut(),
                        &control,
                        &mut input,
                    ),
                )?
                .await?;
            }
        }
        control.check()?;
        Ok((
            task,
            input.edit.expect("completed traversal owns its editor"),
        ))
    })?)
}

impl<'a, E, I> Native<'a, E, I>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
{
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
                work.submit(move |work| async move { run(task, owner, work, edit)?.await })?;
            construction::receive(output).await
        })?)
    }
}

impl<'a, E, I> roots::Input for Native<'a, E, I>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
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
        let value = slot
            .take()
            .edit_from::<Work<'a>, E>(
                identity,
                control,
                &mut self.work,
                self.edit
                    .as_mut()
                    .expect("active traversal owns its editor"),
            )
            .await?;
        slot.restore(value);
        Ok(())
    }
}
impl<'a, E, I> Input for Native<'a, E, I>
where
    E: for<'w> ScalarEdit<Work<'w>, Identity = I> + Send + 'a,
    I: Copy + Send + 'a,
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
        // JOIN ON is inline in SourcePlan; its identity must travel with the detached source.
        // Other scalar roots remain in their original boxes or vector allocations.
        let identity = match value {
            SourcePlan::Join { on: Some(on), .. } => Some(<E as ScalarEditIdentity>::identity(on)),
            _ => None,
        };
        let mut slot = Slot::<MutableSourcePlan>::new(value, memory);
        let task = Task::Source(self.retirement.retain(slot.take()), identity);
        let child = self.child(task, control)?;
        Ok(async move {
            let (task, edit) = child.await?;
            self.edit = Some(edit);
            let Task::Source(value, _) = task else {
                unreachable!("source scalar traversal")
            };
            slot.restore(value.take());
            Ok(())
        })
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
            .submit(move |work| async move { run(task, owner, work, edit)?.await })?;
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
}
