//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Body references are inspected before any admitted argument replaces them.

use crate::{plan::OwnedScalarExpr, SQLError, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn use_counts_with_control(
    expression: &ScalarExpr,
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let mut uses = ProductionVec::new(*control);
    uses.reserve(count)?;
    for _ in 0..count {
        uses.push_copy(0usize)?;
    }
    let mut invalid = false;
    let mut failure = None;
    expression.visit(&mut |expression| {
        if failure.is_some() {
            return;
        }
        if let Err(error) = control.check() {
            failure = Some(error);
            return;
        }
        if let ScalarExpr::Between { expr, .. } = expression {
            expr.visit(&mut |operand| {
                if let ScalarExpr::Param(index) = operand {
                    if let Some(slot) = index
                        .checked_sub(1)
                        .and_then(|index| uses.as_mut_slice().get_mut(index))
                    {
                        *slot = 2;
                    } else {
                        invalid = true;
                    }
                }
            });
        }
        if let ScalarExpr::Param(index) = expression {
            if let Some(slot) = index
                .checked_sub(1)
                .and_then(|index| uses.as_mut_slice().get_mut(index))
            {
                *slot = (*slot + 1).min(2);
            } else {
                invalid = true;
            }
        }
    });
    if let Some(error) = failure {
        return Err(error.into());
    }
    if invalid {
        return Err(SQLError::Internal(
            "SQL body parameter is outside its invocation".into(),
        ));
    }
    Ok(uses.finish()?)
}

pub(super) fn substitute_with<'a>(
    expression: OwnedScalarExpr,
    argument: impl Fn(usize) -> &'a ScalarExpr + Copy,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let expression = expression.map_children(control, |_, child| {
        substitute_with(child, argument, control)
    })?;
    // Post-order replacement never visits a copied caller parameter, whose namespace belongs to the outer query.
    if let ScalarExpr::Param(index) = &*expression {
        return Ok(argument(*index - 1).copy_for_rewrite(control)?);
    }
    Ok(expression)
}
