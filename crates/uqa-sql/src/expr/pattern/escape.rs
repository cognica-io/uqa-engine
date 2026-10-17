//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict `PostgreSQL` escape helpers own their result independently of pattern matching.

use crate::{
    expr::{conversion::value_to_string_with_control, datums::binary_payload, scalar_helpers},
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

pub(in crate::expr) fn evaluate(
    similar: bool,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if !(arguments.len() == 2 || similar && arguments.len() == 1) {
        return Err(SQLError::Internal("invalid pattern escape operands".into()));
    }
    if arguments.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    if !similar {
        if let Some(pattern) = binary_payload(&arguments[0], control)? {
            let escape = binary_payload(&arguments[1], control)?
                .ok_or_else(|| SQLError::Internal("bytea escape has an invalid carrier".into()))?;
            let escape = match escape.bytes() {
                [] => None,
                [escape] => Some(*escape),
                _ => return Err(scalar_helpers::like_pattern::invalid_escape()),
            };
            let mut output = ProductionVec::new(*control);
            normalize(pattern.bytes().iter().copied(), escape, b'\\', |byte| {
                output.push_copy(byte).map_err(Into::into)
            })?;
            let (bytes, memory) = output.finish()?.into_parts();
            return Ok(control.finish(Value::Bytes(bytes), memory)?);
        }
    }
    let pattern = value_to_string_with_control(&arguments[0], control)?;
    let escape = arguments
        .get(1)
        .map(|value| value_to_string_with_control(value, control))
        .transpose()?;
    let escape = escape.as_deref().map(String::as_str);
    let output = if similar {
        scalar_helpers::similar_to_regex_with_control(&pattern, escape, control)?
    } else {
        let escape = scalar_helpers::like_pattern::escape_character(escape)?;
        normalize_text(&pattern, escape, control)?
    };
    let (text, memory) = output.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

/// `like_escape` for text: rewrite `pattern` to use the backslash escape of `escape`, the selected escape character or none.
pub(in crate::expr) fn normalize_text(
    pattern: &str,
    escape: Option<char>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut output = ProductionString::new(*control);
    normalize(pattern.chars(), escape, '\\', |character| {
        output.push(character).map_err(Into::into)
    })?;
    Ok(output.finish()?)
}

fn normalize<T: Copy + Eq>(
    input: impl Iterator<Item = T>,
    escape: Option<T>,
    backslash: T,
    mut push: impl FnMut(T) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut escaped = false;
    for item in input {
        if escape == Some(backslash) || escaped {
            push(item)?;
            escaped = false;
        } else if escape == Some(item) {
            push(backslash)?;
            escaped = true;
        } else {
            if item == backslash {
                push(backslash)?;
            }
            push(item)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
