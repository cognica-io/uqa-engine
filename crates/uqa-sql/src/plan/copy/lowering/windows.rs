//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window definitions share ordered scalar construction and retain partial fields across input suspension.

use super::{
    input::{scalars::construct_scalars, InlineScalarInput, ScalarInput, TextInput},
    Copying, QueryArena, Result,
};
use crate::{
    ast::{Expr, FrameBound, WindowDefinition, WindowSpec},
    plan::{retirement::Pending, AggregateClassifier},
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};

use uqa_core::memory::BudgetedVec;

impl Copying<'_> {
    pub(super) fn lower_windows(
        &mut self,
        windows: &[WindowDefinition],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<WindowDefinition<ScalarWindowSpec>>> {
        crate::catalog::work::inline(construct_windows(
            windows,
            self,
            &mut InlineScalarInput { aggregates, arena },
        ))
        .map(Pending::take)
    }
}

pub(super) async fn construct_windows<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s [WindowDefinition],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Vec<WindowDefinition<ScalarWindowSpec>>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("window allowance"),
    ));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("window fields")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let name = match source.name.as_deref() {
            Some(name) => Some(field.take(input.text(name, copying.control).await?)),
            None => None,
        };
        let mut spec = field.begin_input_field()?;
        let value = construct_window(&source.spec, &mut spec, input).await?;
        field.finish_input_field("spec", spec);
        values
            .push(WindowDefinition {
                name,
                inherited: source.inherited,
                spec: value.take(),
            })
            .expect("admitted window slot");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}

async fn construct_window<'s, I: ScalarInput<&'s Expr>>(
    window: &'s WindowSpec,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<ScalarWindowSpec>> {
    copying.check()?;
    input.checkpoint().await?;
    copying.check()?;
    assert!(
        window.reference.is_none(),
        "named window reference must be resolved before unified-plan lowering"
    );
    let mut field = copying.begin_input_field()?;
    let partition = construct_scalars(
        &window.partition_by,
        &mut field,
        input,
        |expression| expression,
        |_, expression| expression,
    )
    .await?;
    copying.finish_input_field("partition_by", field);
    let mut field = copying.begin_input_field()?;
    let order = construct_scalars(
        &window.order_by,
        &mut field,
        input,
        |order| &order.expr,
        |order, expr| ScalarOrder {
            expr,
            descending: order.descending,
            nulls: order.nulls,
        },
    )
    .await?;
    copying.finish_input_field("order_by", field);
    let mut field = copying.begin_input_field()?;
    let frame = match &window.frame {
        Some(frame) => {
            let mut bound = field.begin_input_field()?;
            let start = construct_bound(&frame.start, &mut bound, input).await?;
            field.finish_input_field("start", bound);
            let mut bound = field.begin_input_field()?;
            let end = construct_bound(&frame.end, &mut bound, input).await?;
            field.finish_input_field("end", bound);
            Pending::new(Some(ScalarWindowFrame {
                mode: frame.mode,
                start: start.take(),
                end: end.take(),
                between: frame.between,
                exclusion: frame.exclusion,
            }))
        }
        None => Pending::new(None),
    };
    copying.finish_input_field("frame", field);
    Ok(Pending::new(ScalarWindowSpec {
        definition: window.definition,
        partition_by: partition.take(),
        order_by: order.take(),
        frame: frame.take(),
    }))
}

async fn construct_bound<'s, I: ScalarInput<&'s Expr>>(
    source: &'s FrameBound,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<ScalarFrameBound>> {
    input.checkpoint().await?;
    copying.check()?;
    let expression = match source {
        FrameBound::UnboundedPreceding => {
            return Ok(Pending::new(ScalarFrameBound::UnboundedPreceding));
        }
        FrameBound::UnboundedFollowing => {
            return Ok(Pending::new(ScalarFrameBound::UnboundedFollowing));
        }
        FrameBound::CurrentRow => return Ok(Pending::new(ScalarFrameBound::CurrentRow)),
        FrameBound::Preceding(expression) | FrameBound::Following(expression) => expression,
    };
    copying.reserve(size_of::<ScalarExpr>())?;
    let expression = input.scalar(expression, copying.control).await?;
    let expression = Box::new(copying.take_scalar(expression)?);
    Ok(Pending::new(
        if matches!(source, FrameBound::Preceding(_)) {
            ScalarFrameBound::Preceding(expression)
        } else {
            ScalarFrameBound::Following(expression)
        },
    ))
}

#[cfg(test)]
mod tests;
