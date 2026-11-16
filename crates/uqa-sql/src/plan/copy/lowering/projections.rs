//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection expressions keep their scalar owner until alias production and destination admission complete.

use super::{
    input::{InlineScalarInput, ScalarInput},
    Copying, QueryArena, Result,
};
use crate::{
    ast::Projection,
    plan::{AggregateClassifier, OwnedProjection, ProjectionPlan},
};
use std::future::{ready, Future};
use uqa_core::memory::{Produced, ProductionControl};

trait ProjectionInput<'s>: ScalarInput<'s> {
    fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>>> + Send;
}

impl<'s> ProjectionInput<'s> for InlineScalarInput<'_, '_> {
    fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>>> + Send {
        ready(control.copy_text(source).map_err(Into::into))
    }
}

impl Copying<'_> {
    pub(super) fn lower_projection(
        &mut self,
        projection: &Projection,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<ProjectionPlan> {
        let projection = crate::catalog::work::inline(construct_projection(
            projection,
            self.control,
            &mut InlineScalarInput { aggregates, arena },
        ))?;
        self.take_projection(projection)
    }
}

async fn construct_projection<'s, I: ProjectionInput<'s>>(
    projection: &'s Projection,
    control: ProductionControl<'_>,
    input: &mut I,
) -> Result<OwnedProjection> {
    control.check()?;
    input.checkpoint().await?;
    let expression = input.scalar(&projection.expr, control).await?;
    let alias = match &projection.alias {
        Some(alias) => Some(input.text(alias, control).await?),
        None => None,
    };
    Ok(OwnedProjection { expression, alias })
}

#[cfg(test)]
mod tests;
