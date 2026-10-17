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

mod matches;
mod preparation;
mod query;
mod replacement;
mod split;
mod substring;
pub use matches::RegexMatches;
pub use split::RegexSplit;
pub use substring::check_constant_similar_translation;
pub(super) use substring::evaluate_dispatched as eval_dispatched_substring;

/// How a function treats the `g` flag once every flag has been validated.
#[derive(Clone, Copy, Debug)]
pub(in crate::expr) enum GlobalFlag {
    /// `regexp_replace` and `regexp_matches` act on every match.
    Accepted,
    /// A single-result function reports `PostgreSQL`'s diagnostic for the option.
    Rejected {
        function: &'static str,
        hint: Option<&'static str>,
    },
}

impl GlobalFlag {
    pub(in crate::expr) const fn rejected_by(function: &'static str) -> Self {
        Self::Rejected {
            function,
            hint: None,
        }
    }
}

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
    global: bool,
}

impl LocalRegex {
    /// Whether the accepted flags requested every match.
    pub(in crate::expr) const fn is_global(&self) -> bool {
        self.global
    }
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
    global_flag: GlobalFlag,
    control: &ProductionControl<'_>,
) -> Result<LocalRegex> {
    let prepared = preparation::prepare(pattern, flags, global_flag, control)?;
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
        global: prepared.global,
    };
    control.check()?;
    Ok(output)
}

/// A scalar regular-expression kernel, selected by function name.
type Kernel = fn(&str, &[Value], &ProductionControl<'_>) -> Result<Produced<Value>>;

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let evaluate: Kernel = match name {
        "regexp_match" => |_, args, control| query::captures(args, control),
        "regexp_split_to_array" => |_, args, control| split::array(args, control),
        "regexp_replace" => replacement::evaluate,
        "regexp_count" | "regexp_instr" | "regexp_like" | "regexp_substr" | "similar_to"
        | "regex_match_op" | "regex_imatch_op" => query::evaluate,
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

/// Where `PostgreSQL`'s search loop (`setup_regexp_matches`, `replace_text_regexp`) looks after `matched`: at its end, or one character later when it is empty, and nowhere once that would pass the end of `input`.
fn next_search_start(input: &str, matched: regex::Match<'_>) -> Option<usize> {
    if matched.is_empty() {
        input[matched.end()..]
            .chars()
            .next()
            .map(|character| matched.end() + character.len_utf8())
    } else {
        Some(matched.end())
    }
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
