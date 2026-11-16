//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immediate scalar slots in the same order as their allocation-aware constructor.

use super::ScalarExpr;
use crate::ir::children::Scope;

pub(super) fn count(expression: &ScalarExpr) -> usize {
    expression.children(Scope::Construction).len()
}

pub(in crate::plan::scalar) fn get_mut(
    expression: &mut ScalarExpr,
    index: usize,
) -> Option<&mut ScalarExpr> {
    expression.children_mut(Scope::Construction).nth(index)
}
