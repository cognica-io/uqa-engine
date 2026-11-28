//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arithmetic type observations belong to a prepared operand and its immutable row schema.

use crate::RowSchema;
use std::sync::OnceLock;
use uqa_core::memory::ProductionControl;
use uqa_sql::{expr::IntegerWidth, type_resolution::ScalarNegationSource, SQLError, ScalarExpr};

pub(in crate::scalar) fn same_schema(left: Option<&RowSchema>, right: Option<&RowSchema>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.shares_index(right),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Debug)]
pub(in crate::scalar) struct SchemaSlot<T>(OnceLock<(Option<RowSchema>, T)>);

impl<T> Default for SchemaSlot<T> {
    fn default() -> Self {
        Self(OnceLock::new())
    }
}

impl<T> SchemaSlot<T> {
    pub(in crate::scalar) fn with_value<R>(
        slot: Option<&Self>,
        schema: Option<&RowSchema>,
        control: &ProductionControl<'_>,
        resolve: impl FnOnce() -> Result<T, SQLError>,
        evaluate: impl FnOnce(&T) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        control.check()?;
        if let Some((saved, value)) = slot.and_then(|slot| slot.0.get()) {
            if same_schema(saved.as_ref(), schema) {
                return evaluate(value);
            }
        }
        #[cfg(test)]
        TYPE_RESOLUTIONS.with(|calls| calls.set(calls.get() + 1));
        let value = resolve()?;
        let result = evaluate(&value)?;
        if let Some(slot) = slot.filter(|slot| slot.0.get().is_none()) {
            let _ = slot.0.set((schema.cloned(), value));
        }
        Ok(result)
    }
}

/// Boxed operands stay at their allocation address while their prepared root moves.
#[derive(Debug)]
pub(in crate::scalar) enum OperandMetadata {
    Arithmetic {
        integer: SchemaSlot<Option<IntegerWidth>>,
        real: SchemaSlot<bool>,
    },
    Negation(SchemaSlot<Option<ScalarNegationSource>>),
}

impl OperandMetadata {
    pub(super) fn for_expression(expression: &ScalarExpr) -> Option<(usize, Self)> {
        let (operand, state) = match expression {
            ScalarExpr::Binary { op, lhs, .. } if uqa_sql::expr::is_arithmetic(*op) => (
                lhs,
                Self::Arithmetic {
                    integer: SchemaSlot::default(),
                    real: SchemaSlot::default(),
                },
            ),
            ScalarExpr::UnaryMinus(inner) => (inner, Self::Negation(SchemaSlot::default())),
            _ => return None,
        };
        Some((std::ptr::from_ref(operand.as_ref()) as usize, state))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::scalar) enum NamedCall {
    Scalar,
    Coalesce,
    Retrieval,
}

/// Only immutable expression properties are prepared. Privileges and session callbacks are checked by the invocation that consumes them.
#[derive(Clone, Copy, Debug)]
pub(in crate::scalar) struct BuiltinCall {
    pub(in crate::scalar) positional: bool,
    pub(in crate::scalar) name: NamedCall,
}

#[derive(Debug)]
pub(in crate::scalar) struct PreparedBuiltin {
    pub(in crate::scalar) call: BuiltinCall,
    pub(in crate::scalar) metadata: uqa_sql::expr::BuiltinCallMetadata,
}

impl BuiltinCall {
    pub(in crate::scalar) fn classify(
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
    ) -> Self {
        #[cfg(test)]
        BUILTIN_CLASSIFICATIONS.with(|count| count.set(count.get() + 1));
        let builtin = binding.is_none_or(|binding| binding.builtin);
        let name_role = if builtin && name.eq_ignore_ascii_case("coalesce") {
            NamedCall::Coalesce
        } else if builtin
            && uqa_sql::semantics::retrieval_function(name)
            && !name.eq_ignore_ascii_case("fts_match")
        {
            NamedCall::Retrieval
        } else {
            NamedCall::Scalar
        };
        Self {
            positional: false,
            name: name_role,
        }
    }

    pub(super) fn prepare(
        name: &str,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[ScalarExpr],
        control: &ProductionControl<'_>,
    ) -> Result<PreparedBuiltin, SQLError> {
        let mut positional = !matches!(args, [ScalarExpr::Star]);
        for argument in args {
            control.check()?;
            if matches!(argument, ScalarExpr::Func { binding, .. } if binding.as_ref().and_then(|binding| binding.dispatch).is_some_and(uqa_sql::ast::FunctionDispatch::is_call_argument_marker))
            {
                positional = false;
                break;
            }
        }
        Ok(PreparedBuiltin {
            call: Self {
                positional,
                ..Self::classify(name, Some(binding))
            },
            metadata: uqa_sql::expr::BuiltinCallMetadata::new(binding),
        })
    }
}

#[cfg(test)]
thread_local! {
    static BUILTIN_CLASSIFICATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TYPE_RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
