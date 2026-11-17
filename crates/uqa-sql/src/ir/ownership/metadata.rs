//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar nodes delegate private type and binding retirement to their existing field owners.

use super::ScalarExpr;
use crate::ColumnType;
use uqa_core::{Value, ValueRetirement};

#[derive(Default)]
pub struct Metadata {
    // Value retirement precedes the final shared owner's reservations during terminal cleanup.
    value: Option<ValueRetirement>,
    literal: Option<crate::ir::retained::ownership::LiteralRetirement>,
}

pub(super) fn advance(value: &mut ScalarExpr, local: &mut Metadata) -> bool {
    if let Some(retirement) = &mut local.value {
        if retirement.advance() {
            local.value = None;
        }
        return false;
    }
    if let Some(literal) = &mut local.literal {
        if literal.advance() {
            local.literal = None;
        }
        return false;
    }
    match value {
        ScalarExpr::Literal(value) => take_value(value, &mut local.value),
        ScalarExpr::RetainedLiteral(value) => {
            local.literal = value.take_retirement();
            local.literal.is_none()
        }
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
            value,
            composite_source,
            bound_type,
            ..
        } => {
            take_value(value, &mut local.value)
                && composite_source
                    .as_deref_mut()
                    .is_none_or(|source| take_value(&mut source.value, &mut local.value))
                && composite_source.as_deref_mut().is_none_or(
                    crate::expr::composites::CompositeConstantSource::retire_metadata_step,
                )
                && bound_type.as_mut().is_none_or(ColumnType::retire_step)
        }
        _ => true,
    }
}

fn take_value(value: &mut Value, local: &mut Option<ValueRetirement>) -> bool {
    if matches!(value, Value::Null) {
        true
    } else {
        *local = Some(ValueRetirement::take(value));
        false
    }
}
