//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute selected pattern syntax through the existing admitted matching kernels.

use crate::{ast::PatternOperator, SQLError};
mod escape;
pub(super) use escape::evaluate as escape;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(super) fn evaluate(
    operator: PatternOperator,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let result = if operator.is_like() {
        super::scalar_core::eval_core_functions_with_control(
            if operator.case_insensitive() {
                "ilike"
            } else {
                "like"
            },
            arguments,
            control,
        )
        .expect("existing LIKE kernel")?
    } else {
        super::regex::evaluate(
            if operator.case_insensitive() {
                "regex_imatch_op"
            } else {
                "regex_match_op"
            },
            arguments,
            control,
        )
        .expect("existing pattern kernel")?
    };
    let (value, memory) = result.into_parts();
    let value = match value {
        Value::Bool(value) => Value::Bool(value != operator.negated()),
        other => other,
    };
    Ok(control.finish(value, memory)?)
}
