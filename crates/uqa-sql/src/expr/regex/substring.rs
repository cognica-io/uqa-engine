//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` `substring` pattern forms. The POSIX form is `textregexsubstr`; the SQL form is defined as `substring($1, similar_to_escape($2, $3))`, so it shares the same extraction after the existing escape translation.

use super::{argument_text, engine, plain, string};
use crate::{
    ast::FunctionDispatch,
    error::{Result, SQLError},
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_pg_regex::{MatchSpan, REG_ADVANCED};

/// The `substring` pattern overload selected by binding; every other dispatch belongs to another kernel. The POSIX form, `textregexsubstr`, matches under the call's input collation. The SQL form is the SQL-standard-body function `substring($1, similar_to_escape($2, $3))`, whose inner call was bound with the database default collation when the body was created, so it matches under that collation whatever the caller's.
pub(in crate::expr) fn evaluate_dispatched(
    dispatch: FunctionDispatch,
    arguments: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let (similar, collation) = match dispatch {
        FunctionDispatch::RegexSubstring => (false, collation),
        FunctionDispatch::SimilarSubstring => (
            true,
            crate::catalog::collations::DEFAULT_COLLATION_OID as u32,
        ),
        _ => return None,
    };
    Some(evaluate(similar, arguments, collation, control))
}

fn evaluate(
    similar: bool,
    arguments: &[Value],
    collation: u32,
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
        let input = argument_text(&arguments[0], control)?;
        let pattern = argument_text(&arguments[1], control)?;
        return extract(&input, &pattern, collation, control);
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
    let input = argument_text(&arguments[0], control)?;
    extract(&input, pattern, collation, control)
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

/// `textregexsubstr`: the first parenthesized subexpression when the pattern has one, otherwise the whole match. No match, or a first subexpression that did not take part in the match, yields NULL. The pattern is compiled before the input is converted.
fn extract(
    input: &str,
    pattern: &str,
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let call = engine::RegexCall::new(control);
    let mut allowance = call.allowance();
    let mut regex = call.compile(pattern, REG_ADVANCED, collation, &mut allowance)?;
    let points = engine::code_points(input, control)?;
    let mut spans = [MatchSpan::UNSET; 2];
    let found = call.execute(&mut regex, &points, 0, &mut spans)?;
    let selected = if regex.subexpression_count() > 0 {
        spans[1]
    } else {
        spans[0]
    };
    drop(regex);
    match selected.range().filter(|_| found) {
        Some(range) => string(engine::text(&points, range, control)?, control),
        None => plain(Value::Null, control),
    }
}
