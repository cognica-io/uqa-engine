//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL properties of one immutable builtin call. The executing session supplies privileges on each invocation.

use crate::{ast::FunctionBinding, error::Result, ColumnType};
use std::sync::OnceLock;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// The prepared expression owner supplies this binding's metadata while the expression is alive.
pub trait PreparedBuiltinCalls {
    fn builtin_metadata(&self, binding: &FunctionBinding) -> Option<&BuiltinCallMetadata>;
}

#[derive(Debug)]
pub struct BuiltinCallMetadata {
    returns_set: bool,
    numeric_result: OnceLock<Option<NumericWidth>>,
}

impl BuiltinCallMetadata {
    #[must_use]
    pub fn new(binding: &FunctionBinding) -> Self {
        Self {
            returns_set: crate::semantics::sets::static_setness::builtin_call_returns_set(
                &binding.name,
                Some(binding),
            ),
            numeric_result: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn returns_set(&self) -> bool {
        self.returns_set
    }

    /// Resolve the signature only after this call produces a numeric value. Failed or cancelled type resolution never publishes a result.
    pub(in crate::expr) fn numeric_result(
        site: Option<&Self>,
        binding: &FunctionBinding,
        control: &ProductionControl<'_>,
    ) -> Result<Option<NumericWidth>> {
        control.check()?;
        if let Some(result) = site.and_then(|site| site.numeric_result.get()) {
            return Ok(*result);
        }
        let result = match crate::type_resolution::bound_result_type(binding, control)? {
            Some(ColumnType::SmallInteger) => Some(NumericWidth::SmallInteger),
            Some(ColumnType::Integer) => Some(NumericWidth::Integer),
            Some(ColumnType::Real) => Some(NumericWidth::Real),
            _ => None,
        };
        if let Some(site) = site {
            let _ = site.numeric_result.set(result);
        }
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::expr) enum NumericWidth {
    SmallInteger,
    Integer,
    Real,
}

impl NumericWidth {
    pub(in crate::expr) fn coerce(
        self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        let value = match self {
            Self::SmallInteger => crate::expr::casting::cast_integer(value, "smallint", control)?,
            Self::Integer => crate::expr::casting::cast_integer(value, "integer", control)?,
            Self::Real => Value::Float(crate::expr::floating::to_float_with_control(
                value,
                crate::expr::FloatWidth::Real,
                control,
            )?),
        };
        Ok(control.finish(value, control.empty_reservation())?)
    }
}

#[cfg(test)]
mod tests;
