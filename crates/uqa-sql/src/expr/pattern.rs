//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute selected pattern syntax through the existing admitted matching kernels.

use crate::{ast::PatternOperator, SQLError};
mod escape;
pub(super) use escape::evaluate as escape;
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
    let name = match (operator.is_like(), operator.case_insensitive()) {
        (true, false) => "like",
        (true, true) => "ilike",
        (false, false) => "regex_match_op",
        (false, true) => "regex_imatch_op",
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
