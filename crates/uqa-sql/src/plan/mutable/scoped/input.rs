//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline visitors and scheduled binding share the same scoped child traversal.

use super::{
    command, query, source, MutableCommandPlan, MutableQueryPlan, MutableSourcePlan,
    OwnedScopedPlanVisitor, ProductionControl, SQLError, Scope,
};
use crate::{
    catalog::work::{inline, Work},
    plan::mutable::edits::Editable,
};
use std::future::{ready, Future};

type Result<T> = std::result::Result<T, SQLError>;

pub(super) trait Input {
    type Work: Work + Send;
    fn work(&mut self) -> &mut Self::Work;
    fn query(
        &mut self,
        value: MutableQueryPlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableQueryPlan>> + Send>;
    fn command(
        &mut self,
        value: MutableCommandPlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableCommandPlan>> + Send>;
    fn source(
        &mut self,
        value: MutableSourcePlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableSourcePlan>> + Send>;
    fn source_enter(
        &mut self,
        value: &mut MutableSourcePlan,
        scope: &Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<bool>> + Send>;
    fn command_enter(
        &mut self,
        value: &mut MutableCommandPlan,
        scope: &Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<bool>> + Send>;
    fn query_complete(
        &mut self,
        value: &mut MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send>;
    fn visible(
        &mut self,
        scope: &Scope,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<bool>> + Send;
}

pub(super) struct Immediate<'a> {
    outer: &'a dyn Fn(&str) -> bool,
    visitor: &'a mut dyn OwnedScopedPlanVisitor,
    work: (),
}
impl<'a> Immediate<'a> {
    pub(super) fn new(
        outer: &'a dyn Fn(&str) -> bool,
        visitor: &'a mut dyn OwnedScopedPlanVisitor,
    ) -> Self {
        Self {
            outer,
            visitor,
            work: (),
        }
    }
}

fn take<T: Editable>(value: &mut T) -> T {
    std::mem::replace(value, T::from_parts(T::empty(), None))
}

impl Input for Immediate<'_> {
    type Work = ();
    fn work(&mut self) -> &mut Self::Work {
        &mut self.work
    }
    fn query(
        &mut self,
        mut value: MutableQueryPlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableQueryPlan>> + Send> {
        inline(query(&mut value, scope, control, self))?;
        Ok(ready(Ok(value)))
    }
    fn command(
        &mut self,
        mut value: MutableCommandPlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableCommandPlan>> + Send> {
        inline(command(&mut value, scope, control, self))?;
        Ok(ready(Ok(value)))
    }
    fn source(
        &mut self,
        mut value: MutableSourcePlan,
        scope: Scope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableSourcePlan>> + Send> {
        inline(source(&mut value, scope, control, self))?;
        Ok(ready(Ok(value)))
    }
    fn source_enter(
        &mut self,
        value: &mut MutableSourcePlan,
        scope: &Scope,
        _control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<bool>> + Send> {
        let (replacement, descend) = self.visitor.source(take(value), &|name| {
            scope.visible(name) || (scope.external && (self.outer)(name))
        })?;
        *value = replacement;
        Ok(ready(Ok(descend)))
    }
    fn command_enter(
        &mut self,
        value: &mut MutableCommandPlan,
        scope: &Scope,
        _control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<bool>> + Send> {
        let (replacement, descend) = self.visitor.command(take(value), &|name| {
            scope.visible(name) || (scope.external && (self.outer)(name))
        })?;
        *value = replacement;
        Ok(ready(Ok(descend)))
    }
    fn query_complete(
        &mut self,
        value: &mut MutableQueryPlan,
        _control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<()>> + Send> {
        *value = self.visitor.query_complete(take(value))?;
        Ok(ready(Ok(())))
    }
    fn visible(
        &mut self,
        scope: &Scope,
        name: &str,
        _control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<bool>> + Send {
        ready(Ok(
            scope.visible(name) || (scope.external && (self.outer)(name))
        ))
    }
}

mod native;
