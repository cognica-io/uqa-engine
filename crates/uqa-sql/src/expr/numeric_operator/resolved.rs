//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-size numeric overloads retain typed coercions, never input values or invocation controls.

use super::{
    ColumnType, NumericOperator, Produced, ProductionControl, ProductionVec, Result, SQLError,
    Value,
};
use crate::{expr::NumericCast, type_resolution::NumericOperatorTypes};

/// A selected builtin numeric operation. Every evaluation still converts all operands in order and uses the invoking memory and cancellation controls.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedNumericOperator {
    operator: NumericOperator,
    arguments: [Option<NumericCast>; 2],
    result: NumericCast,
}

impl ResolvedNumericOperator {
    pub(super) fn from_selection(
        operator: NumericOperator,
        sources: &[Option<ColumnType>],
        selected: &NumericOperatorTypes,
    ) -> Option<Self> {
        let mut arguments = [None; 2];
        for (index, (source, target)) in sources.iter().zip(&selected.arguments).enumerate() {
            arguments[index] = Some(NumericCast::select(source.as_ref()?, target)?);
        }
        Some(Self {
            operator,
            arguments,
            result: NumericCast::select(&selected.result, &selected.result)?,
        })
    }

    pub fn evaluate(
        &self,
        arguments: &[Value],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        control.check()?;
        if arguments.len() != self.operator.arity() {
            return Err(SQLError::Internal(
                "resolved operator operand count differs".into(),
            ));
        }
        let mut converted = ProductionVec::new(*control);
        converted.reserve(arguments.len())?;
        for (value, conversion) in arguments.iter().zip(self.arguments) {
            converted.push_produced(
                conversion
                    .expect("selected numeric operand")
                    .evaluate(value, control)?,
            )?;
        }
        let arguments = converted.finish()?;
        let value = super::evaluate_converted(
            self.operator,
            &arguments,
            &self.result.target_type(),
            control,
        )?;
        // The shared value carrier does not encode int2/int4 or real width.
        self.result.evaluate(&value, control)
    }
}

#[cfg(test)]
mod tests;
