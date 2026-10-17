//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` `substring` pattern forms. The POSIX form is `textregexsubstr`; the SQL form is defined as `substring($1, similar_to_escape($2, $3))`, so it shares the same extraction after the existing escape translation.

use super::{compile, plain, string};
use crate::{
    ast::FunctionDispatch,
    error::{Result, SQLError},
    expr::conversion::value_to_string_with_control,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// The `substring` pattern overload selected by binding; every other dispatch belongs to another kernel.
pub(in crate::expr) fn evaluate_dispatched(
    dispatch: FunctionDispatch,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let similar = match dispatch {
        FunctionDispatch::RegexSubstring => false,
        FunctionDispatch::SimilarSubstring => true,
        _ => return None,
    };
    Some(evaluate(similar, arguments, control))
}

fn evaluate(
    similar: bool,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if arguments.len() != if similar { 3 } else { 2 } {
        return Err(SQLError::Internal(
            "substring pattern form received the wrong operand count".into(),
        ));
    }
    // Both PostgreSQL forms are strict.
    if arguments.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    let input = value_to_string_with_control(&arguments[0], control)?;
    if !similar {
        let pattern = value_to_string_with_control(&arguments[1], control)?;
        return extract(&input, &pattern, control);
    }
    let escaped = crate::expr::pattern::escape(true, &arguments[1..], control)?;
    let Value::Str(pattern) = &*escaped else {
        return Err(SQLError::Internal(
            "similar_to_escape produced a non-text pattern".into(),
        ));
    };
    extract(&input, pattern, control)
}

/// `textregexsubstr`: the first parenthesized subexpression when the pattern has one, otherwise the whole match. No match, or a first subexpression that did not participate in the match, yields NULL.
fn extract(input: &str, pattern: &str, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let regex = compile(pattern, "", false, control)?;
    let captures = regex.captures(input);
    control.check()?;
    let Some(captures) = captures else {
        return plain(Value::Null, control);
    };
    let selected = if regex.captures_len() > 1 {
        captures.get(1)
    } else {
        captures.get(0)
    };
    match selected {
        Some(matched) => string(control.copy_text(matched.as_str())?, control),
        None => plain(Value::Null, control),
    }
}
