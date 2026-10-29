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
