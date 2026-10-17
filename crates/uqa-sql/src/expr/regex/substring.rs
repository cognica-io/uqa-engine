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
    if !similar {
        // `textregexsubstr` is strict.
        if arguments.iter().any(|value| matches!(value, Value::Null)) {
            return plain(Value::Null, control);
        }
        let input = value_to_string_with_control(&arguments[0], control)?;
        let pattern = value_to_string_with_control(&arguments[1], control)?;
        return extract(&input, &pattern, control);
    }
    // The planner folds a constant NULL argument of this strict form before execution. At run time PostgreSQL evaluates its inlined body, `textregexsubstr($1, similar_to_escape($2, $3))`: a NULL pattern or escape yields NULL, the translation runs and can fail even for a NULL input, and only the extraction is skipped for that input.
    let Some(escaped) = translate(&arguments[1..], control)? else {
        return plain(Value::Null, control);
    };
    let Value::Str(pattern) = &*escaped else {
        return Err(SQLError::Internal(
            "similar_to_escape produced a non-text pattern".into(),
        ));
    };
    if matches!(arguments[0], Value::Null) {
        return plain(Value::Null, control);
    }
    let input = value_to_string_with_control(&arguments[0], control)?;
    extract(&input, pattern, control)
}

/// Translate the SQL form's constant pattern and escape while planning. `PostgreSQL` inlines `substring(text, text, text)` and folds a constant `similar_to_escape` call, so its errors surface even when no row is read; the call itself is unchanged.
pub fn check_constant_similar_translation(
    pattern_and_escape: &[Value],
    control: &ProductionControl<'_>,
) -> Result<()> {
    translate(pattern_and_escape, control).map(drop)
}

/// `similar_to_escape(pattern, escape)`, or `None` when either strict operand is NULL.
fn translate(
    pattern_and_escape: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if pattern_and_escape
        .iter()
        .any(|value| matches!(value, Value::Null))
    {
        return Ok(None);
    }
    crate::expr::pattern::escape(true, pattern_and_escape, control).map(Some)
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
