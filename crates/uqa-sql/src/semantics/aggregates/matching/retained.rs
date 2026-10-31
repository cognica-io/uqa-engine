//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained identity keeps both decoded values owned through comparison and preserves conservative semantic-read failure.

use crate::ir::retained::observation::observe;
use crate::{ir::RetainedScalarLiteral, SQLError, ScalarExpr};
use uqa_core::memory::ProductionControl;

pub(super) fn same(
    left: &RetainedScalarLiteral,
    right: &RetainedScalarLiteral,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    left.has_same_representation_with_control(right, control)
}

pub(super) fn inline(
    literal: &RetainedScalarLiteral,
    other: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(observe(&literal.value, control)?
        .is_some_and(|value| super::retained_value_matches(literal, &value, other)))
}
