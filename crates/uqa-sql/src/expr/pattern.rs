//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute selected pattern syntax through the existing admitted matching kernels.

use crate::{ast::PatternOperator, SQLError};
mod escape;
pub(super) use escape::evaluate as escape;
pub(super) use escape::evaluate_similar as escape_similar;
pub(super) use escape::normalize_text as normalize_escape;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// Evaluate a selected pattern operator under the call's input collation, `PostgreSQL`'s `inputcollid` (zero when indeterminate).
pub(super) fn evaluate(
    operator: PatternOperator,
    arguments: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let name = match operator {
        PatternOperator::Like | PatternOperator::NotLike => "like",
        PatternOperator::ILike | PatternOperator::NotILike => "ilike",
        PatternOperator::Regex | PatternOperator::NotRegex => "regex_match_op",
        PatternOperator::RegexInsensitive | PatternOperator::NotRegexInsensitive => {
            "regex_imatch_op"
        }
        PatternOperator::StartsWith => "starts_with",
    };
    let result = super::scalar_core::eval_collated_functions_with_control(
        name, arguments, collation, control,
    )
    .expect("existing pattern kernel")?;
    let (value, memory) = result.into_parts();
    let value = match value {
        Value::Bool(value) => Value::Bool(value != operator.negated()),
        other => other,
    };
    Ok(control.finish(value, memory)?)
}
