//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh retained statements schedule the same scalar traversal under their original retirement owner.

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
        CommandPlan, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        MutableSourcePlan, OwnedScalarExpr, QueryPlan, SourcePlan,
    },
    semantics::view_rewrite::DeferredOuterRewrite,
    SQLError, ScalarExpr,
};
use std::future::{ready, Future};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{OwnedProductionControl, Produced, ProductionControl},
};

type Resolver<'a> =
    dyn Fn(&str, &ProductionControl<'_>) -> Result<Produced<String>> + Send + Sync + 'a;

struct Native<'a> {
    work: Work<'a>,
    retirement: Context,
    resolve: &'a Resolver<'a>,
}
enum Task {
    Query(Prefix<MutableQueryPlan>),
    Command(Prefix<MutableCommandPlan>),
    Source(Prefix<MutableSourcePlan>),
}

fn run<'a>(
    mut task: Task,
    owner: OwnedProductionControl,
    work: Work<'a>,
    resolve: &'a Resolver<'a>,
) -> Result<Read<'a, Result<Task>>> {
    let admission = owner.clone();
    Ok(Read::new(admission.control(), async move {
        let retirement = match &task {
            Task::Query(value) => value.context(),
            Task::Command(value) => value.context(),
            Task::Source(value) => value.context(),
        }
        .clone();
        let mut input = Native {
            work,
            retirement,
            resolve,
        };
        let control = owner.control();
        match &mut task {
            Task::Query(value) => {
                let value = &mut **value;
                query_from(
                    &mut value.query,
                    value.memory.as_mut(),
                    &control,
                    &mut input,
                )
                .await?;
            }
            Task::Command(value) => {
                let value = &mut **value;
                command_from(
                    &mut value.command,
                    value.memory.as_mut(),
                    &control,
                    &mut input,
                    true,
                )
                .await?;
            }
            Task::Source(value) => {
                let value = &mut **value;
                source_from(
                    &mut value.source,
                    value.memory.as_mut(),
                    &control,
                    &mut input,
                )
                .await?;
            }
        }
        control.check()?;
        Ok(task)
    })?)
}

impl<'a> Native<'a> {
    fn child(
        &mut self,
        task: Task,
        control: &ProductionControl<'_>,
    ) -> Result<Read<'a, Result<Task>>> {
        let owner = control.capture();
        let mut work = self.work.clone();
        let resolve = self.resolve;
        Ok(Read::new(*control, async move {
            let output =
                work.submit(move |work| async move { run(task, owner, work, resolve)?.await })?;
            construction::receive(output).await
        })?)
    }
}

impl<'a> roots::Input for Native<'a> {
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
        let mut slot = Slot::<OwnedScalarExpr>::new(expression, memory);
        let value = slot
            .take()
            .bind_sequence_references_from(&mut self.resolve, control, &mut self.work)
            .await?;
        slot.restore(value);
        Ok(())
    }
}
impl Input for Native<'_> {
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
            let Task::Query(value) = child.await? else {
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
        let mut slot = Slot::<MutableSourcePlan>::new(value, memory);
        let task = Task::Source(self.retirement.retain(slot.take()));
        let child = self.child(task, control)?;
        Ok(async move {
            let Task::Source(value) = child.await? else {
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
            let Task::Command(value) = child.await? else {
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
    pub(in crate::plan) async fn bind_sequence_names_from_input(
        self,
        resolve: &Resolver<'_>,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self> {
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
            .submit(move |work| async move { run(task, owner, work, resolve)?.await })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        control.check()?;
        Ok(match construction::receive(output).await? {
            Task::Query(value) => value.map(MutableExecutablePlan::Query),
            Task::Command(value) => value.map(MutableExecutablePlan::Command),
            Task::Source(_) => unreachable!("executable binding root"),
        })
    }
}
