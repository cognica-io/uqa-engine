//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::{
    expressions,
    input::{ScalarInput, TextInput},
};
use super::{Copying, Result};
use crate::{
    ast::{AssignmentTargets, Expr, MultipleAssignmentTargets},
    plan::{retirement::Pending, AssignmentPlan},
    ScalarExpr,
};
use uqa_core::memory::BudgetedVec;

pub(super) async fn construct_assignments<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s [(AssignmentTargets, Expr)],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Vec<AssignmentPlan>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut output = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("assignment allowance"),
    ));
    output.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("assignment records")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let assignment = construct_assignment(source, &mut field, input).await?;
        output
            .push(assignment.take())
            .expect("admitted assignment slot");
        copying.finish_input_field("item", field);
    }
    let (output, memory) = output.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(output))
}

async fn construct_assignment<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s (AssignmentTargets, Expr),
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<AssignmentPlan>> {
    let mut field = copying.begin_input_field()?;
    let target = match &source.0 {
        AssignmentTargets::Single(target) => Pending::new(AssignmentTargets::Single(
            super::targets::construct_target(target, &mut field, input)
                .await?
                .take(),
        )),
        AssignmentTargets::Multiple(group) => {
            let mut targets = field.begin_input_field()?;
            let values =
                super::targets::construct_targets(&group.targets, &mut targets, input).await?;
            field.finish_input_field("targets", targets);
            let mut positions =
                BudgetedVec::new(copying.control.budget().expect("assignment positions"));
            positions.reserve(group.source_positions.len())?;
            field
                .fields
                .as_mut()
                .expect("position records")
                .reserve(group.source_positions.len())?;
            for position in &group.source_positions {
                input.checkpoint().await?;
                copying.check()?;
                let item = field.begin_input_field()?;
                positions
                    .push(*position)
                    .expect("admitted assignment position");
                field.finish_input_field("item", item);
            }
            let (positions, memory) = positions.into_parts();
            field.absorb(Some(memory));
            Pending::new(AssignmentTargets::Multiple(MultipleAssignmentTargets {
                targets: values.take(),
                source_positions: positions,
                source_width: group.source_width,
            }))
        }
    };
    copying.finish_input_field("target", field);
    let mut value_field = copying.begin_input_field()?;
    let value = input.scalar(&source.1, copying.control).await?;
    let value = value_field.take_scalar(value)?;
    copying.finish_input_field("value", value_field);
    Ok(Pending::new(AssignmentPlan {
        target: target.take(),
        value,
    }))
}

pub(super) async fn optional<'s, I: ScalarInput<&'s Expr>>(
    name: &'static str,
    source: Option<&'s Expr>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Option<ScalarExpr>>> {
    let mut field = copying.begin_input_field()?;
    let value = expressions::construct_optional_scalar(source, &mut field, input).await?;
    copying.finish_input_field(name, field);
    Ok(value)
}

pub(super) async fn optional_boxed<'s, I: ScalarInput<&'s Expr>>(
    name: &'static str,
    source: Option<&'s Expr>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Option<Box<ScalarExpr>>>> {
    let mut field = copying.begin_input_field()?;
    let value = expressions::construct_optional_scalar_box(source, &mut field, input).await?;
    copying.finish_input_field(name, field);
    Ok(value)
}

pub(super) use super::super::super::input::text::{optional_text, texts};
