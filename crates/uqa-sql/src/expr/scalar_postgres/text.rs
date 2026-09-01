//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{to_i64, Result, SQLError, Value};

pub(super) fn invalid_regex_parameter(name: &str, value: i64) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("invalid value for parameter \"{name}\": {value}"),
    }
}

pub(super) fn positive_regex_parameter(
    value: Option<&Value>,
    default: usize,
    name: &str,
) -> Result<usize> {
    let value = value.map(to_i64).transpose()?.unwrap_or(default as i64);
    if value <= 0 {
        return Err(invalid_regex_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

pub(super) fn nonnegative_regex_parameter(
    value: Option<&Value>,
    default: usize,
    name: &str,
) -> Result<usize> {
    let value = value.map(to_i64).transpose()?.unwrap_or(default as i64);
    if value < 0 {
        return Err(invalid_regex_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

pub(super) fn regex_tail(string: &str, start: usize) -> Option<(&str, usize)> {
    let base_chars = start.checked_sub(1)?;
    if base_chars == 0 {
        return Some((string, 0));
    }
    let byte_index = string
        .char_indices()
        .nth(base_chars)
        .map(|(index, _)| index)?;
    Some((&string[byte_index..], base_chars))
}
