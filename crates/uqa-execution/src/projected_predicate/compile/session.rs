//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A positional evaluator without session hooks can only select session-independent operators and casts.

use crate::{RowSchema, ScalarExpr};
use uqa_sql::{
    ast::{BinaryOp, FunctionDispatch, FunctionVolatility},
    ColumnType, SQLError, SQLParam,
};

pub(super) fn required(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let pair = |op, left, right| {
        let left = uqa_sql::scalar_type_with_control(left, schema, params, control)?;
        let right = uqa_sql::scalar_type_with_control(right, schema, params, control)?;
        needs_session(op, left.as_deref(), right.as_deref(), control)
    };
    match expression {
        ScalarExpr::Cast { expr, .. } => {
            let source = uqa_sql::scalar_type_with_control(expr, schema, params, control)?;
            let target = uqa_sql::scalar_type_with_control(expression, schema, params, control)?;
            match source.zip(target) {
                Some((source, target)) => {
                    Ok(uqa_sql::type_resolution::cast_volatility_with_control(
                        &source, &target, control,
                    )? != FunctionVolatility::Immutable)
                }
                None => Ok(true),
            }
        }
        ScalarExpr::Binary { op, lhs, rhs } => pair(*op, lhs, rhs),
        ScalarExpr::Between { expr, low, high } => {
            Ok(pair(BinaryOp::GreaterEqual, expr, low)? || pair(BinaryOp::LessEqual, expr, high)?)
        }
        ScalarExpr::InList { expr, list, .. } => {
            for item in list {
                if pair(BinaryOp::Equal, expr, item)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            ..
        } if matches!(
            binding.dispatch,
            Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator)
        ) =>
        {
            let [left, right, ..] = args.as_slice() else {
                return Ok(false);
            };
            let left = uqa_sql::scalar_type_with_control(left, schema, params, control)?;
            let right = uqa_sql::scalar_type_with_control(right, schema, params, control)?;
            let Some(ColumnType::Array(element)) = right.as_deref() else {
                return Ok(false);
            };
            needs_session(BinaryOp::Equal, left.as_deref(), Some(element), control)
        }
        _ => Ok(false),
    }
}

fn needs_session(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    match uqa_sql::type_resolution::binary_operator_volatility_with_control(
        op, left, right, control,
    ) {
        Ok(volatility) => Ok(volatility != FunctionVolatility::Immutable),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(false),
    }
}
