//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture, count, position and predicate results of `regexp.c`, which retain only admitted SQL values.

use super::{
    argument_text, engine, flag_text,
    flags::ReFlags,
    invalid_parameter,
    match_set::{MatchSet, Search},
    parameter, plain, string, SingleResult,
};
use crate::error::{Result, SQLError};
use crate::expr::out_of_range;
use std::borrow::Cow;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_pg_regex::{CompileFlags, REG_ADVANCED, REG_ICASE, REG_NOSUB};

/// `regexp_match`: the capture array of the first match, or NULL. `regexp_matches` returns rows through [`super::RegexMatches`].
pub(super) fn captures(
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "regexp_match takes 2 or 3 args".into(),
        ));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    let input = argument_text(&args[0], control)?;
    let pattern = argument_text(&args[1], control)?;
    let flags = ReFlags::parse(&flag_text(args.get(2), control)?)?.reject_global(SingleResult {
        function: "regexp_match",
        hint: Some("Use the regexp_matches function instead."),
    })?;
    let search = Search {
        start: 0,
        subexpressions: true,
        ignore_degenerate: false,
    };
    let matches = MatchSet::find(&input, &pattern, flags, collation, search, control)?;
    if matches.len() == 0 {
        return plain(Value::Null, control);
    }
    matches.row(0, control)
}

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if name == "similar_to" {
        return similar(args, collation, control);
    }
    let (maximum, message) = match name {
        "regexp_count" => (4, "regexp_count takes 2-4 args"),
        "regexp_instr" => (7, "regexp_instr takes 2-7 args"),
        "regexp_like" => (3, "regexp_like takes 2-3 args"),
        "regex_match_op" | "regex_imatch_op" => (2, "a regular expression match takes 2 args"),
        "regexp_substr" => (6, "regexp_substr takes 2-6 args"),
        _ => unreachable!("regular-expression family membership was checked"),
    };
    if !(2..=maximum).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(message.into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    let input = if matches!(name, "regex_match_op" | "regex_imatch_op") {
        pattern_input(&args[0], control)?
    } else {
        argument_text(&args[0], control)?
    };
    let pattern = argument_text(&args[1], control)?;
    match name {
        "regexp_count" => count(&input, &pattern, args, collation, control),
        "regexp_instr" | "regexp_substr" => {
            selected(name, &input, &pattern, args, collation, control)
        }
        "regexp_like" => {
            let flags = ReFlags::parse(&flag_text(args.get(2), control)?)?
                .reject_global(SingleResult::named("regexp_like"))?;
            matches_anywhere(&input, &pattern, flags.cflags, collation, control)
        }
        // `textregexeq` and `texticregexeq`.
        "regex_match_op" => matches_anywhere(&input, &pattern, REG_ADVANCED, collation, control),
        "regex_imatch_op" => matches_anywhere(
            &input,
            &pattern,
            REG_ADVANCED | REG_ICASE,
            collation,
            control,
        ),
        _ => unreachable!("regular-expression family membership was checked"),
    }
}

/// `RE_compile_and_execute` without subexpressions: whether `pattern` matches anywhere in `input`. The pattern is compiled before the input is converted.
fn matches_anywhere(
    input: &str,
    pattern: &str,
    cflags: CompileFlags,
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let call = engine::RegexCall::new(control);
    let mut allowance = call.allowance();
    let mut regex = call.compile(pattern, cflags | REG_NOSUB, collation, &mut allowance)?;
    let points = engine::code_points(input, control)?;
    let found = call.execute(&mut regex, &points, 0, &mut [])?;
    drop(regex);
    plain(Value::Bool(found), control)
}

/// `regexp_count`: the number of matches from character `start`, empty matches included.
fn count(
    input: &str,
    pattern: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let start = parameter(args.get(2), 1, 1, "start", control)?;
    let flags = ReFlags::parse(&flag_text(args.get(3), control)?)?
        .reject_global(SingleResult::named("regexp_count"))?
        .every_match();
    let search = Search {
        start: start - 1,
        subexpressions: false,
        ignore_degenerate: false,
    };
    let matches = MatchSet::find(input, pattern, flags, collation, search, control)?;
    let count = i64::try_from(matches.len()).map_err(|_| out_of_range("integer"))?;
    plain(Value::Int(count), control)
}

/// `regexp_instr` and `regexp_substr`: the position or text of match `n` from character `start`, or of its subexpression `subexpr`. Every parameter is validated before the flags, and the flags before the pattern.
fn selected(
    name: &str,
    input: &str,
    pattern: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let position = name == "regexp_instr";
    let start = parameter(args.get(2), 1, 1, "start", control)?;
    let occurrence = parameter(args.get(3), 1, 1, "n", control)?;
    let end_option = if position {
        let value = args
            .get(4)
            .map(|value| crate::expr::conversion::to_i64_with_control(value, control))
            .transpose()?
            .unwrap_or(0);
        if !matches!(value, 0 | 1) {
            return Err(invalid_parameter("endoption", value));
        }
        value
    } else {
        0
    };
    let subexpression = parameter(
        args.get(if position { 6 } else { 5 }),
        0,
        0,
        "subexpr",
        control,
    )?;
    let function = if position {
        "regexp_instr"
    } else {
        "regexp_substr"
    };
    let flags = ReFlags::parse(&flag_text(args.get(if position { 5 } else { 4 }), control)?)?
        .reject_global(SingleResult::named(function))?
        .every_match();
    let search = Search {
        start: start - 1,
        subexpressions: subexpression > 0,
        ignore_degenerate: false,
    };
    let matches = MatchSet::find(input, pattern, flags, collation, search, control)?;
    let absent = || plain(if position { Value::Int(0) } else { Value::Null }, control);
    // `PostgreSQL` records the whole match as the only pattern when the expression has no subexpressions, so subexpression 1 then names the whole match.
    if occurrence > matches.len() || subexpression > matches.patterns() {
        return absent();
    }
    let span = matches.span(occurrence - 1, subexpression.saturating_sub(1));
    if position {
        let offset = if end_option == 1 {
            span.end()
        } else {
            span.start()
        };
        let Some(offset) = offset else {
            return absent();
        };
        let position = offset
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| out_of_range("integer"))?;
        return plain(Value::Int(position), control);
    }
    match matches.text(span, control)? {
        Some(text) => string(text, control),
        None => absent(),
    }
}

/// `SIMILAR TO`: `textregexeq(input, similar_to_escape(pattern, escape))`. The strict translation runs, and can fail, before a NULL input yields NULL.
fn similar(
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if !matches!(args.len(), 2 | 3) {
        return Err(SQLError::TypeMismatch(
            "similar_to takes 2 or 3 args".into(),
        ));
    }
    if matches!(args[1], Value::Null) || matches!(args.get(2), Some(Value::Null)) {
        return plain(Value::Null, control);
    }
    let escape = args
        .get(2)
        .map(|value| argument_text(value, control))
        .transpose()?;
    let input_pattern = argument_text(&args[1], control)?;
    let pattern = crate::expr::scalar_helpers::similar_to_regex_with_control(
        &input_pattern,
        escape.as_deref().map(AsRef::as_ref),
        control,
    )?;
    if matches!(args[0], Value::Null) {
        return plain(Value::Null, control);
    }
    let input = pattern_input(&args[0], control)?;
    matches_anywhere(&input, &pattern, REG_ADVANCED, collation, control)
}

/// The text an operator matches: a `character(n)` value keeps its padding, as `bpcharregexeq` reads the stored value.
fn pattern_input<'v>(
    value: &'v Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Cow<'v, str>>> {
    match value {
        Value::FixedChar(text) => control
            .finish(Cow::Borrowed(text.as_str()), control.empty_reservation())
            .map_err(Into::into),
        Value::Datum(datum) => {
            let stored = crate::expr::datums::read_with_control(datum, control)?;
            let (text, memory) = control
                .copy_text(&pattern_input(&stored, control)?)?
                .into_parts();
            control.finish(Cow::Owned(text), memory).map_err(Into::into)
        }
        _ => argument_text(value, control),
    }
}
