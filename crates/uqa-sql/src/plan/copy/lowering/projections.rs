//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection expressions keep their scalar owner until alias production and destination admission complete.

use super::{
    input::{InlineScalarInput, ScalarInput, TextInput},
    Copying, QueryArena, Result,
};
use crate::{
    ast::Projection,
    plan::{retirement::Pending, AggregateClassifier, OwnedProjection, ProjectionPlan},
};
use uqa_core::memory::{BudgetedVec, ProductionControl};

impl Copying<'_> {
    pub(super) fn lower_projections(
        &mut self,
        projections: &[Projection],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<ProjectionPlan>> {
        crate::catalog::work::inline(construct_projections(
            projections,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }
}

async fn construct_projection<'s, I: ScalarInput<&'s crate::ast::Expr> + TextInput<&'s str>>(
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

pub(super) async fn construct_projections<
    's,
    I: ScalarInput<&'s crate::ast::Expr> + TextInput<&'s str>,
>(
    source: &'s [Projection],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Vec<ProjectionPlan>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("projection allowance"),
    ));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("projection fields")
        .reserve(source.len())?;
    for source in source {
        let mut field = copying.begin_input_field()?;
        let projection = construct_projection(source, copying.control, input).await?;
        values
            .push(field.take_projection(projection)?)
            .expect("admitted projection slot");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}

#[cfg(test)]
mod tests;
