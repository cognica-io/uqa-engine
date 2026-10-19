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
) -> Result<bool, SQLError> {
    let pair = |op, left, right| {
        let left = crate::scalar_type(left, schema, params)?;
        let right = crate::scalar_type(right, schema, params)?;
        Ok(needs_session(op, left.as_ref(), right.as_ref()))
    };
    match expression {
        ScalarExpr::Cast { expr, .. } => {
            let source = crate::scalar_type(expr, schema, params)?;
            let target = crate::scalar_type(expression, schema, params)?;
            Ok(source.zip(target).is_none_or(|(source, target)| {
                uqa_sql::type_resolution::cast_volatility(&source, &target)
                    != FunctionVolatility::Immutable
            }))
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
            let left = crate::scalar_type(left, schema, params)?;
            let right = crate::scalar_type(right, schema, params)?;
            let Some(ColumnType::Array(element)) = right else {
                return Ok(false);
            };
            Ok(needs_session(
                BinaryOp::Equal,
                left.as_ref(),
                Some(&element),
            ))
        }
        _ => Ok(false),
    }
}

fn needs_session(op: BinaryOp, left: Option<&ColumnType>, right: Option<&ColumnType>) -> bool {
    uqa_sql::type_resolution::binary_operator_volatility(op, left, right)
        .is_ok_and(|volatility| volatility != FunctionVolatility::Immutable)
}
