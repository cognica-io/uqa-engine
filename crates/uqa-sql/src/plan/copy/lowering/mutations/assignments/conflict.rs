//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::input::{scalars::construct_scalars, ScalarInput, TextInput};
use super::{input, Copying, Result};
use crate::{
    ast::{Expr, OnConflict, OnConflictAction},
    plan::{retirement::Pending, ConflictActionPlan, ConflictPlan},
};

pub(in crate::plan::copy::lowering::mutations) async fn construct_conflict<
    's,
    I: ScalarInput<&'s Expr> + TextInput<&'s str>,
>(
    source: &'s OnConflict,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<ConflictPlan>> {
    input.checkpoint().await?;
    copying.check()?;
    // The action precedes inference predicates and columns in ordinary lowering.
    let mut field = copying.begin_input_field()?;
    let action = construct_action(&source.action, &mut field, input).await?;
    copying.finish_input_field("action", field);
    let predicate =
        input::optional_boxed("predicate", source.predicate.as_deref(), copying, input).await?;
    let constraint = input::optional_text(source.constraint.as_deref(), copying, input).await?;
    let mut field = copying.begin_input_field()?;
    let conflict_columns = input::texts(&source.conflict_columns, &mut field, input).await?;
    copying.finish_input_field("conflict_columns", field);
    let mut field = copying.begin_input_field()?;
    let expressions = construct_scalars(
        &source.expressions,
        &mut field,
        input,
        |value| value,
        |_, value| value,
    )
    .await?;
    copying.finish_input_field("expressions", field);
    Ok(Pending::new(ConflictPlan {
        predicate: predicate.take(),
        constraint,
        conflict_columns,
        expressions: expressions.take(),
        action: action.take(),
    }))
}

async fn construct_action<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s OnConflictAction,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<ConflictActionPlan>> {
    match source {
        OnConflictAction::Nothing => Ok(Pending::new(ConflictActionPlan::Nothing)),
        OnConflictAction::Update {
            assignments,
            r#where,
        } => {
            let mut field = copying.begin_input_field()?;
            let assignments = input::construct_assignments(assignments, &mut field, input).await?;
            copying.finish_input_field("assignments", field);
            let predicate =
                input::optional_boxed("predicate", r#where.as_deref(), copying, input).await?;
            Ok(Pending::new(ConflictActionPlan::Update {
                assignments: assignments.take(),
                predicate: predicate.take(),
            }))
        }
    }
}
