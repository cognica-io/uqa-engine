//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Symbolic invocation argument materialization and parameter substitution.

use super::RoutineInliningContext;
use crate::{
    ast::{CreateFunction, RoutineInvocationBinding},
    SQLError, ScalarExpr,
};

pub(super) fn prepare(
    context: &RoutineInliningContext<'_>,
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    arguments: &[ScalarExpr],
) -> Result<Option<Vec<ScalarExpr>>, SQLError> {
    super::owned_arguments::prepare(
        context,
        definition,
        invocation,
        arguments,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .map(super::owned_arguments::OwnedArguments::into_uncontrolled)
    .transpose()
}

pub(super) fn use_counts(expression: &ScalarExpr, count: usize) -> Result<Vec<usize>, SQLError> {
    super::owned_substitution::use_counts_with_control(
        expression,
        count,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|uses| {
        uses.into_uncontrolled()
            .expect("ordinary parameter use counts")
    })
}

pub(super) fn substitute(expression: &mut ScalarExpr, arguments: &[ScalarExpr]) {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let input = crate::plan::OwnedScalarExpr::uncontrolled(std::mem::replace(
        expression,
        ScalarExpr::Literal(uqa_core::Value::Null),
    ));
    *expression =
        super::owned_substitution::substitute_with(input, |index| &arguments[index], &control)
            .expect("ordinary parameter substitution")
            .finish(&control)
            .expect("ordinary substituted expression")
            .into_uncontrolled()
            .expect("ordinary substituted expression has no reservation");
}
