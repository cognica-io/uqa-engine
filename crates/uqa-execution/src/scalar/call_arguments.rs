//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime evaluation of validated SQL call arguments.

use super::{eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_core::Value;
use uqa_sql::SQLError;

pub use uqa_sql::ir::{
    scalar_call_argument, scalar_call_arguments, validate_scalar_call_arguments, ScalarCallArgument,
};

pub fn eval_call_arguments(
    arguments: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    scalar_call_arguments(arguments)?
        .into_iter()
        .map(|argument| {
            Ok((
                argument.name.map(str::to_string),
                eval_scalar(argument.value, context)?,
            ))
        })
        .collect()
}
