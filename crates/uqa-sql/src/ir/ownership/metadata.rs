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
pub enum Metadata {
    #[default]
    Empty,
    Value(ValueRetirement),
    Literal(crate::ir::retained::ownership::LiteralRetirement),
}

pub(super) fn advance(value: &mut ScalarExpr, local: &mut Metadata) -> bool {
    match local {
        Metadata::Value(retirement) => {
            if retirement.advance() {
                *local = Metadata::Empty;
            }
            return false;
        }
        Metadata::Literal(literal) => {
            if literal.advance() {
                *local = Metadata::Empty;
            }
            return false;
        }
        Metadata::Empty => (),
    }
    match value {
        ScalarExpr::Literal(value) => take_value(value, local),
        ScalarExpr::RetainedLiteral(value) => match value.take_retirement() {
            Some(literal) => {
                *local = Metadata::Literal(literal);
                false
            }
            None => true,
        },
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
            take_value(value, local)
                && composite_source
                    .as_deref_mut()
                    .is_none_or(|source| take_value(&mut source.value, local))
                && composite_source.as_deref_mut().is_none_or(
                    crate::expr::composites::CompositeConstantSource::retire_metadata_step,
                )
                && bound_type.as_mut().is_none_or(ColumnType::retire_step)
        }
        _ => true,
    }
}

fn take_value(value: &mut Value, local: &mut Metadata) -> bool {
    if matches!(value, Value::Null) {
        true
    } else {
        *local = Metadata::Value(ValueRetirement::take(value));
        false
    }
}
