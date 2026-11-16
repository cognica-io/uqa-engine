//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar nodes delegate private type and binding retirement to their existing field owners.

use super::ScalarExpr;
use crate::ColumnType;

pub(super) fn advance(value: &mut ScalarExpr) -> bool {
    match value {
        ScalarExpr::Func { binding, .. } => binding
            .as_mut()
            .is_none_or(crate::ast::FunctionBinding::retire_step),
        ScalarExpr::Collated { ty, .. } => ty.retire_step(),
        ScalarExpr::CompositeRow {
            binding,
            bound_type,
            ..
        } => binding.retire_step() && bound_type.as_mut().is_none_or(ColumnType::retire_step),
        ScalarExpr::TypedLiteral {
            composite_source,
            bound_type,
            ..
        } => {
            composite_source
                .as_deref_mut()
                .is_none_or(crate::expr::composites::CompositeConstantSource::retire_metadata_step)
                && bound_type.as_mut().is_none_or(ColumnType::retire_step)
        }
        _ => true,
    }
}
