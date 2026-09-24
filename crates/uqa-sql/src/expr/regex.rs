//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL regular-expression result production. Native compiled state and search caches are local to one call; only admitted `Value` payloads cross the result boundary.

use crate::error::{Result, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod preparation;
mod query;
mod replacement;

#[derive(Clone, Copy, Debug)]
struct Options {
    case_insensitive: bool,
    multi_line: bool,
    dot_matches_new_line: bool,
}

/// No global cache or retained-row back-reference owns this native compiler result. Its private search pool dies with this call, before the admitted input pattern. This owner does not claim exact allocator-byte accounting for opaque native automata.
#[derive(Debug)]
pub(in crate::expr) struct LocalRegex {
    regex: regex::Regex,
    _pattern: Produced<String>,
}

impl std::ops::Deref for LocalRegex {
    type Target = regex::Regex;

    fn deref(&self) -> &Self::Target {
        &self.regex
    }
}

pub(in crate::expr) fn compile(
    pattern: &str,
    flags: &str,
    global_allowed: bool,
    control: &ProductionControl<'_>,
) -> Result<LocalRegex> {
    let prepared = preparation::prepare(pattern, flags, global_allowed, control)?;
    control.check()?;
    let mut builder = regex::RegexBuilder::new(&prepared.pattern);
    builder
        .case_insensitive(prepared.options.case_insensitive)
        .multi_line(prepared.options.multi_line)
        .dot_matches_new_line(prepared.options.dot_matches_new_line);
    let regex = builder.build().map_err(|error| SQLError::Routine {
        sqlstate: "2201B".into(),
        message: format!("invalid regular expression: {error}"),
    })?;
    let output = LocalRegex {
        regex,
        _pattern: prepared.pattern,
    };
    control.check()?;
    Ok(output)
}

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let evaluate = match name {
        "regexp_match" | "regexp_matches" => query::captures,
        "regexp_replace" => replacement::evaluate,
        "regexp_count" | "regexp_instr" | "regexp_like" | "regexp_substr" | "similar_to" => {
            query::evaluate
        }
        _ => return None,
    };
    Some(
        control
            .check()
            .map_err(SQLError::from)
            .and_then(|()| evaluate(name, args, control)),
    )
}

fn plain(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    control
        .finish(value, control.empty_reservation())
        .map_err(Into::into)
}

fn string(value: Produced<String>, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    control
        .finish(Value::Str(value), memory)
        .map_err(Into::into)
}

fn parameter(
    value: Option<&Value>,
    default: usize,
    minimum: i64,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<usize> {
    let value = value
        .map(|value| super::conversion::to_i64_with_control(value, control))
        .transpose()?
        .unwrap_or(default as i64);
    if value < minimum {
        return Err(invalid_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

fn invalid_parameter(name: &str, value: i64) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("invalid value for parameter \"{name}\": {value}"),
    }
}

fn flags(value: Option<&Value>, control: &ProductionControl<'_>) -> Result<Produced<String>> {
    value.map_or_else(
        || control.copy_text("").map_err(Into::into),
        |value| super::conversion::value_to_string_with_control(value, control),
    )
}

fn tail<'a>(
    string: &'a str,
    start: usize,
    control: &ProductionControl<'_>,
) -> Result<Option<(&'a str, usize)>> {
    let Some(base_chars) = start.checked_sub(1) else {
        return Ok(None);
    };
    if base_chars == 0 {
        return Ok(Some((string, 0)));
    }
    for (offset, (byte, _)) in string.char_indices().enumerate() {
        control.check()?;
        if offset == base_chars {
            return Ok(Some((&string[byte..], base_chars)));
        }
    }
    Ok(None)
}

fn nth_capture<'h>(
    regex: &regex::Regex,
    haystack: &'h str,
    occurrence: usize,
    control: &ProductionControl<'_>,
) -> Result<Option<regex::Captures<'h>>> {
    for (index, capture) in regex.captures_iter(haystack).enumerate() {
        control.check()?;
        if index == occurrence - 1 {
            return Ok(Some(capture));
        }
    }
    control.check()?;
    Ok(None)
}

#[cfg(test)]
mod tests;
