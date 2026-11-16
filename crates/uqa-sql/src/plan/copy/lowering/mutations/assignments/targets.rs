//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::input::{ScalarInput, TextInput};
use super::{Copying, Result};
use crate::{
    ast::{AssignmentStep, AssignmentTarget, Expr},
    plan::retirement::Pending,
    ScalarExpr,
};
use uqa_core::memory::BudgetedVec;

pub(super) async fn construct_targets<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s [AssignmentTarget],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Vec<AssignmentTarget<ScalarExpr>>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("target allowance"),
    ));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("target records")
        .reserve(source.len())?;
    for target in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let target = construct_target(target, &mut field, input).await?;
        values.push(target.take()).expect("admitted target slot");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}

pub(super) async fn construct_target<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s AssignmentTarget,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<AssignmentTarget<ScalarExpr>>> {
    input.checkpoint().await?;
    copying.check()?;
    let column = copying.take(input.text(&source.column, copying.control).await?);
    let mut field = copying.begin_input_field()?;
    let mut steps = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("target steps"),
    ));
    steps.reserve(source.indirection.len())?;
    field
        .fields
        .as_mut()
        .expect("target step records")
        .reserve(source.indirection.len())?;
    for source in &source.indirection {
        input.checkpoint().await?;
        let mut item = field.begin_input_field()?;
        let step = match source {
            AssignmentStep::Field(name) => {
                AssignmentStep::Field(item.take(input.text(name, copying.control).await?))
            }
            AssignmentStep::Index(value) => AssignmentStep::Index(
                super::input::optional_boxed("index", Some(value), &mut item, input)
                    .await?
                    .take()
                    .expect("present index"),
            ),
            AssignmentStep::Slice { lower, upper } => {
                let lower =
                    super::input::optional_boxed("lower", lower.as_deref(), &mut item, input)
                        .await?;
                let upper =
                    super::input::optional_boxed("upper", upper.as_deref(), &mut item, input)
                        .await?;
                AssignmentStep::Slice {
                    lower: lower.take(),
                    upper: upper.take(),
                }
            }
        };
        steps.push(step).expect("admitted target step");
        field.finish_input_field("item", item);
    }
    let (steps, memory) = steps.take().into_parts();
    field.absorb(Some(memory));
    copying.finish_input_field("indirection", field);
    Ok(Pending::new(AssignmentTarget {
        column,
        indirection: steps,
    }))
}
