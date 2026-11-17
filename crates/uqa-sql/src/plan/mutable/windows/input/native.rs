//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained plans schedule the existing normalization with independent child tasks.

use super::{
    command, query, query_fields, source, Context, Future, Input, MutableCommandPlan,
    MutableQueryPlan, MutableSourcePlan, OwnedScalarExpr, ProductionControl, Result, SourceScope,
    Windows,
};
use crate::{
    construction::{self, Read, Work},
    plan::retirement::construction::Prefix,
};
use uqa_core::memory::OwnedProductionControl;

struct Native<'a> {
    work: Work<'a>,
    retirement: Context,
}

enum Task {
    Query(Prefix<MutableQueryPlan>),
    Command(Prefix<MutableCommandPlan>),
    Source(Prefix<MutableSourcePlan>, SourceScope),
}

fn run(
    mut task: Task,
    owner: OwnedProductionControl,
    work: Work<'_>,
) -> Result<Read<'_, Result<Task>>> {
    let admission = owner.clone();
    Ok(Read::new(admission.control(), async move {
        let retirement = match &task {
            Task::Query(value) => value.context(),
            Task::Command(value) => value.context(),
            Task::Source(value, _) => value.context(),
        }
        .clone();
        let mut input = Native { work, retirement };
        let control = owner.control();
        match &mut task {
            Task::Query(value) => query(value, &control, &mut input).await?,
            Task::Command(value) => command(value, &control, &mut input).await?,
            Task::Source(value, scope) => {
                source(value, scope.clone(), &control, &mut input).await?;
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
        Ok(Read::new(*control, async move {
            let output = work.submit(move |work| async move { run(task, owner, work)?.await })?;
            construction::receive(output).await
        })?)
    }
}

impl<'a> Input for Native<'a> {
    type Work = Work<'a>;
    fn work(&mut self) -> &mut Self::Work {
        &mut self.work
    }
    fn retirement(&self) -> Option<&Context> {
        Some(&self.retirement)
    }
    fn scalar(
        &mut self,
        value: OwnedScalarExpr,
        resolved: Windows,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<OwnedScalarExpr>> + Send> {
        let owner = control.capture();
        let output = self.work.submit(move |mut work| async move {
            value
                .normalize_window_definitions_from(&resolved, &owner.control(), &mut work)
                .await
        })?;
        Ok(construction::receive(output))
    }
    fn query(
        &mut self,
        value: MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableQueryPlan>> + Send> {
        let task = Task::Query(self.retirement.retain(value));
        let task = self.child(task, control)?;
        Ok(async move {
            let Task::Query(value) = task.await? else {
                unreachable!("query normalization")
            };
            Ok(value.take())
        })
    }
    fn command(
        &mut self,
        value: MutableCommandPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableCommandPlan>> + Send> {
        let task = Task::Command(self.retirement.retain(value));
        let task = self.child(task, control)?;
        Ok(async move {
            let Task::Command(value) = task.await? else {
                unreachable!("command normalization")
            };
            Ok(value.take())
        })
    }
    fn source(
        &mut self,
        value: MutableSourcePlan,
        scope: SourceScope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableSourcePlan>> + Send> {
        let task = Task::Source(self.retirement.retain(value), scope);
        let task = self.child(task, control)?;
        Ok(async move {
            let Task::Source(value, _) = task.await? else {
                unreachable!("source normalization")
            };
            Ok(value.take())
        })
    }
}

impl Prefix<MutableQueryPlan> {
    /// Child query constructors have already completed their own normalization. Visit only this query's shared scalar scope and arena before returning to its parent.
    pub(in crate::plan) fn normalize_local_window_definitions_from<'a>(
        mut self,
        control: &ProductionControl<'_>,
        work: Work<'a>,
    ) -> Result<Read<'a, Result<Self>>> {
        let owner = control.capture();
        Ok(Read::new(*control, async move {
            let mut input = Native {
                work,
                retirement: self.context().clone(),
            };
            let control = owner.control();
            query_fields(&mut self, &control, &mut input).await?;
            control.check()?;
            Ok(self)
        })?)
    }
}
