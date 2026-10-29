//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volatility, strictness and repeated-argument cost of analyzed expressions.

use super::RoutineInliningContext;
use crate::{
    ast::{CreateFunction, FunctionVolatility},
    RowSchema, SQLError, SQLParam, ScalarExpr,
};

pub(super) struct Properties {
    volatility: FunctionVolatility,
    nonstrict: bool,
    subquery: bool,
    set_returning: bool,
    cost: f64,
}

impl Properties {
    pub(super) fn permits(&self, definition: &CreateFunction) -> bool {
        !self.subquery
            && !self.set_returning
            && rank(self.volatility) <= rank(definition.volatility)
            && (!definition.strict || !self.nonstrict)
    }
}

const fn rank(volatility: FunctionVolatility) -> u8 {
    match volatility {
        FunctionVolatility::Immutable => 0,
        FunctionVolatility::Stable => 1,
        FunctionVolatility::Volatile => 2,
    }
}

mod production;
pub(super) use production::{can_duplicate_with_control, inspect_with_control};

pub(super) fn volatility(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    schema: &RowSchema,
) -> Result<FunctionVolatility, SQLError> {
    Ok(inspect_with_control(
        context,
        expression,
        &[],
        schema,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .volatility)
}

pub(super) fn volatility_with_control(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    schema: &RowSchema,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    Ok(inspect_with_control(context, expression, &[], schema, control)?.volatility)
}
