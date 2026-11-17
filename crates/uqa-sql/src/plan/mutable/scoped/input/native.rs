//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored names schedule the shared scoped traversal while detached children keep their original retirement owner.

use super::{
    command, query, source, Future, Input, MutableCommandPlan, MutableQueryPlan, MutableSourcePlan,
    ProductionControl, Result, Scope,
};
use crate::{
    binding::view_dependencies::owned::fields,
    catalog::work::Work as CatalogWork,
    construction::{self, Read, Work},
    plan::{
        retirement::construction::{Context, Prefix},
        MutableExecutablePlan, SourcePlan,
    },
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{OwnedProductionControl, Produced},
};

use fields::StoredRelationResolver as Resolver;

struct Native<'a> {
    work: Work<'a>,
    retirement: Context,
    resolve: &'a Resolver<'a>,
    temporary: bool,
}

enum Task {
    Query(Prefix<MutableQueryPlan>),
    Command(Prefix<MutableCommandPlan>),
    Source(Prefix<MutableSourcePlan>),
}

fn run<'a>(
    mut task: Task,
    scope: Scope,
    owner: OwnedProductionControl,
    work: Work<'a>,
    resolve: &'a Resolver<'a>,
) -> Result<Read<'a, Result<(Task, bool)>>> {
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
            temporary: false,
        };
        let control = owner.control();
        match &mut task {
            Task::Query(value) => query(value, scope, &control, &mut input).await?,
            Task::Command(value) => command(value, scope, &control, &mut input).await?,
            Task::Source(value) => source(value, scope, &control, &mut input).await?,
        }
        control.check()?;
        Ok((task, input.temporary))
    })?)
}

impl<'a> Native<'a> {
    fn child(
        &mut self,
        task: Task,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<Read<'a, Result<(Task, bool)>>> {
        let owner = control.capture();
        let mut work = self.work.clone();
        let resolve = self.resolve;
        Ok(Read::new(*control, async move {
            let output = work
                .submit(move |work| async move { run(task, scope, owner, work, resolve)?.await })?;
            construction::receive(output).await
        })?)
    }
}

impl<'a> Input for Native<'a> {
    type Work = Work<'a>;
    fn work(&mut self) -> &mut Self::Work {
        &mut self.work
    }
    fn query(
        &mut self,
        value: MutableQueryPlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableQueryPlan>> + Send> {
        let task = Task::Query(self.retirement.retain(value));
        let child = self.child(task, scope, control)?;
        Ok(async move {
            let (Task::Query(value), temporary) = child.await? else {
                unreachable!("query binding")
            };
            self.temporary |= temporary;
            Ok(value.take())
        })
    }
    fn command(
        &mut self,
        value: MutableCommandPlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableCommandPlan>> + Send> {
        let task = Task::Command(self.retirement.retain(value));
        let child = self.child(task, scope, control)?;
        Ok(async move {
            let (Task::Command(value), temporary) = child.await? else {
                unreachable!("command binding")
            };
            self.temporary |= temporary;
            Ok(value.take())
        })
    }
    fn source(
        &mut self,
        value: MutableSourcePlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableSourcePlan>> + Send> {
        let task = Task::Source(self.retirement.retain(value));
        let child = self.child(task, scope, control)?;
        Ok(async move {
            let (Task::Source(value), temporary) = child.await? else {
                unreachable!("source binding")
            };
            self.temporary |= temporary;
            Ok(value.take())
        })
    }
    fn source_enter(
        &mut self,
        value: &mut MutableSourcePlan,
        scope: &Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<bool>> + Send> {
        Ok(async move {
            fields::source(value, control, &mut Fields { input: self, scope }).await?;
            Ok(!matches!(&**value, SourcePlan::Table { .. }))
        })
    }
    fn command_enter(
        &mut self,
        value: &mut MutableCommandPlan,
        scope: &Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<bool>> + Send> {
        Ok(async move {
            fields::command(value, control, &mut Fields { input: self, scope }).await?;
            Ok(true)
        })
    }
    fn query_complete(
        &mut self,
        value: &mut MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        Ok(std::future::ready(
            value.set_relations_bound_in_place(true, control),
        ))
    }
    async fn visible(
        &mut self,
        scope: &Scope,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        scope.contains(name, control, &mut self.work).await
    }
}

struct Fields<'a, 'r> {
    input: &'a mut Native<'r>,
    scope: &'a Scope,
}
impl<'r> fields::Input for Fields<'_, 'r> {
    type Work = Work<'r>;
    fn work(&mut self) -> &mut Self::Work {
        &mut self.input.work
    }
    async fn visible(&mut self, name: &str, control: &ProductionControl<'_>) -> Result<bool> {
        self.scope
            .contains(name, control, &mut self.input.work)
            .await
    }
    async fn resolve(
        &mut self,
        name: &str,
        descendants: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        control.check()?;
        CatalogWork::checkpoint(&mut self.input.work).await?;
        control.check()?;
        let (name, temporary) = (self.input.resolve)(name, descendants, control)?;
        self.input.temporary |= temporary;
        Ok(name)
    }
}

impl Prefix<MutableExecutablePlan> {
    pub(in crate::plan) async fn bind_relation_names_from_input(
        self,
        resolve: &Resolver<'_>,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(Self, bool)> {
        control.check()?;
        input.checkpoint().await?;
        let mut driver = construction::Driver::new(*control)?;
        let context = self.context().clone();
        let task = match self.take() {
            MutableExecutablePlan::Query(value) => Task::Query(context.retain(value)),
            MutableExecutablePlan::Command(value) => Task::Command(context.retain(value)),
        };
        let owner = control.capture();
        let output = driver.work().submit(move |work| async move {
            run(task, Scope::root(), owner, work, resolve)?.await
        })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        control.check()?;
        let (task, temporary) = construction::receive(output).await?;
        let plan = match task {
            Task::Query(value) => value.map(MutableExecutablePlan::Query),
            Task::Command(value) => value.map(MutableExecutablePlan::Command),
            Task::Source(_) => unreachable!("executable binding root"),
        };
        Ok((plan, temporary))
    }
}
