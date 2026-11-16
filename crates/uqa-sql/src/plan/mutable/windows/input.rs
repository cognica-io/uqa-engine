//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural normalization shares semantics while native child jobs retain their constructor's retirement context.

use super::{
    command, query, source, MutableCommandPlan, MutableQueryPlan, MutableSourcePlan, SourceScope,
    Windows,
};
use crate::{
    catalog::work::Work as CatalogWork,
    plan::{
        retirement::construction::{Context, Root},
        scalar::{OwnedScalarExpr, ScalarInputWork},
    },
    schema::retention::CatalogRetentionError,
    SQLError,
};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

type Result<T> = std::result::Result<T, SQLError>;

pub(super) trait Input: Send {
    type Work: ScalarInputWork<Error: From<CatalogRetentionError> + Into<SQLError>> + CatalogWork;
    fn work(&mut self) -> &mut Self::Work;
    fn scalar(
        &mut self,
        value: OwnedScalarExpr,
        resolved: Windows,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<OwnedScalarExpr>> + Send>;
    fn query(
        &mut self,
        value: MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableQueryPlan>> + Send>;
    fn command(
        &mut self,
        value: MutableCommandPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableCommandPlan>> + Send>;
    fn source(
        &mut self,
        value: MutableSourcePlan,
        scope: SourceScope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableSourcePlan>> + Send>;
}

pub(super) struct Immediate {
    retirement: Option<Context>,
    work: (),
}

impl Immediate {
    pub(super) fn new(retirement: Option<Context>) -> Self {
        Self {
            retirement,
            work: (),
        }
    }
}

fn immediate<T: Root>(
    mut value: T,
    retirement: Option<Context>,
    control: &ProductionControl<'_>,
    normalize: impl FnOnce(&mut T) -> Result<()>,
) -> Result<T> {
    if let Some(retirement) = retirement {
        let mut value = retirement.retain(value);
        normalize(&mut value)?;
        control.check()?;
        Ok(value.take())
    } else {
        normalize(&mut value)?;
        control.check()?;
        Ok(value)
    }
}

impl Input for Immediate {
    type Work = ();
    fn work(&mut self) -> &mut Self::Work {
        &mut self.work
    }
    fn scalar(
        &mut self,
        value: OwnedScalarExpr,
        resolved: Windows,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<OwnedScalarExpr>> + Send> {
        Ok(ready(crate::catalog::work::inline(
            value.normalize_window_definitions_from(&resolved, control, &mut self.work),
        )))
    }
    fn query(
        &mut self,
        value: MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableQueryPlan>> + Send> {
        Ok(ready(immediate(
            value,
            self.retirement.clone(),
            control,
            |value| crate::catalog::work::inline(query(value, control, self)),
        )))
    }
    fn command(
        &mut self,
        value: MutableCommandPlan,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableCommandPlan>> + Send> {
        Ok(ready(immediate(
            value,
            self.retirement.clone(),
            control,
            |value| crate::catalog::work::inline(command(value, control, self)),
        )))
    }
    fn source(
        &mut self,
        value: MutableSourcePlan,
        scope: SourceScope,
        control: &ProductionControl<'_>,
    ) -> Result<impl Future<Output = Result<MutableSourcePlan>> + Send> {
        Ok(ready(immediate(
            value,
            self.retirement.clone(),
            control,
            |value| crate::catalog::work::inline(source(value, scope, control, self)),
        )))
    }
}

#[cfg(test)]
mod tests;
