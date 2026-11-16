//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen scalars keep iterative retirement until their final fallible publication completes.

use super::{
    folding::Folding, OwnedScalarExpr, Produced, ProductionControl, Result, ScalarExpr,
    ScalarMemory,
};
use crate::plan::{
    retirement::{Pending, Retire},
    scalar::resources::input::Work,
};

pub(in crate::plan::scalar) struct Publication<T: Retire> {
    // The scalar retires before the folded lease on cancellation or abandoned publication.
    value: Pending<T>,
    memory: Option<Folding>,
}

impl Publication<ScalarExpr> {
    pub(super) fn new(
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        expression.assert_owner(control);
        control.check()?;
        let (value, memory) = expression.into_rewrite_parts();
        crate::catalog::work::inline(
            Self::from_parts(Pending::new(value), memory).prepare_from(control, &mut ()),
        )
    }
}

impl<T: Retire> Publication<T> {
    pub(in crate::plan::scalar) fn from_parts(
        value: Pending<T>,
        memory: Option<ScalarMemory>,
    ) -> Self {
        Self {
            value,
            memory: memory.map(Folding::new),
        }
    }

    pub(in crate::plan::scalar) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<T>> {
        crate::catalog::work::inline(self.finish_from(control, &mut ()))
    }

    pub(in crate::plan::scalar) async fn finish_from<W: Work>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Produced<T>, W::Error> {
        let ready = self.prepare_from(control, work).await?;
        Ok(ready
            .value
            .finish(ready.memory.map(Folding::finish), control)?)
    }

    async fn prepare_from<W: Work>(
        mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error> {
        if let Some(memory) = &mut self.memory {
            while !memory.is_complete() {
                control.check()?;
                work.checkpoint().await?;
                control.check()?;
                memory.advance();
            }
        }
        Ok(self)
    }
}
