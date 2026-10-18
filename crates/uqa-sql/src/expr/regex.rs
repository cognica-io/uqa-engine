//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL regular expressions, evaluated by `PostgreSQL` 18.4's Advanced Regular Expression engine as `src/backend/utils/adt/regexp.c` calls it. Compiled expressions and their working memory are local to one call and charged to its allowance; only admitted `Value` payloads cross the result boundary.

use crate::error::{Result, SQLError};
use std::borrow::Cow;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod engine;
mod flags;
mod match_set;
mod matches;
mod query;
mod replacement;
mod split;
mod substring;
pub use matches::RegexMatches;
pub use split::RegexSplit;
pub use substring::check_constant_similar_translation;
pub(super) use substring::evaluate_dispatched as eval_dispatched_substring;

/// A function that returns a single result and so rejects the `g` flag with `PostgreSQL`'s diagnostic once every flag has been validated. `regexp_replace` and `regexp_matches` accept `g`.
#[derive(Clone, Copy, Debug)]
struct SingleResult {
    function: &'static str,
    hint: Option<&'static str>,
}

impl SingleResult {
    const fn named(function: &'static str) -> Self {
        Self {
            function,
            hint: None,
        }
    }
}

/// A scalar regular-expression kernel, selected by function name, with the call's input collation.
type Kernel = fn(&str, &[Value], u32, &ProductionControl<'_>) -> Result<Produced<Value>>;

/// Evaluate a scalar regular-expression function or operator. `collation` is the call's input collation, `PostgreSQL`'s `inputcollid`: the selected collation OID, or zero when it is indeterminate.
pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let evaluate: Kernel = match name {
        "regexp_match" => |_, args, collation, control| query::captures(args, collation, control),
        "regexp_split_to_array" => {
            |_, args, collation, control| split::array(args, collation, control)
        }
        "regexp_replace" => replacement::evaluate,
        "regexp_count" | "regexp_instr" | "regexp_like" | "regexp_substr" | "similar_to"
        | "regex_match_op" | "regex_imatch_op" => query::evaluate,
        _ => return None,
    };
    Some(
        control
            .check()
            .map_err(SQLError::from)
            .and_then(|()| evaluate(name, args, collation, control)),
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

/// The text of an argument: borrowed where the value holds text, and otherwise written as its cast to `text` writes it, so a call admits no second copy of a text argument.
fn argument_text<'v>(
    value: &'v Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Cow<'v, str>>> {
    control.check()?;
    if let Value::Str(text) = value {
        return control
            .finish(Cow::Borrowed(text.as_str()), control.empty_reservation())
            .map_err(Into::into);
    }
    let (text, memory) =
        super::conversion::value_to_string_with_control(value, control)?.into_parts();
    control.finish(Cow::Owned(text), memory).map_err(Into::into)
}

/// The text of an optional flags argument; a call without one uses no flags.
fn flag_text<'v>(
    value: Option<&'v Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Cow<'v, str>>> {
    match value {
        Some(value) => argument_text(value, control),
        None => control
            .finish(Cow::Borrowed(""), control.empty_reservation())
            .map_err(Into::into),
    }
}

#[cfg(test)]
mod tests;
