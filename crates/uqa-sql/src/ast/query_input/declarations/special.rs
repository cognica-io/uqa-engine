//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{AutoIncrement, SequenceRestart};
use uqa_core::catalog_role::RoleIdentity;

pub(super) fn read_estimate<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    _: &Values<'_>,
) -> Result<Native<Option<f32>>, InputError> {
    let mut child = input.content_child(token)?;
    let value = match child.next()? {
        JsonToken::Null => None,
        JsonToken::Number(text) => Some(if text.starts_with('-') {
            crate::ast::stored_input::number::<i64>(JsonToken::Number(text))? as f32
        } else {
            crate::ast::stored_input::number::<u64>(JsonToken::Number(text))? as f32
        }),
        token @ JsonToken::String(_) => Some(match child.text(token)?.as_str() {
            "Infinity" => f32::INFINITY,
            "-Infinity" => f32::NEG_INFINITY,
            "NaN" => f32::NAN,
            _ => return Err(invalid()),
        }),
        _ => return Err(invalid()),
    };
    child.end()?;
    Native::new(input.scalar(value)?, input.control)
}

pub(super) fn read_owner<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    values: &Values<'_>,
) -> Result<Native<Option<RoleIdentity>>, InputError> {
    let mut child = input.content_child(token)?;
    let token = child.next()?;
    let result = if matches!(token, JsonToken::String(_)) {
        if !child.text(token)?.is_empty() {
            return Err(invalid());
        }
        Native::new(child.scalar(None)?, child.control)?
    } else {
        child.stored(token, values)?
    };
    child.end()?;
    Ok(result)
}

pub(super) fn read_auto_increment<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    values: &Values<'_>,
) -> Result<Native<Option<AutoIncrement>>, InputError> {
    let mut child = input.content_child(token)?;
    let token = child.next()?;
    let mut parts = Parts::new(input.control);
    let result = match token {
        JsonToken::Null | JsonToken::Bool(false) => None,
        JsonToken::Bool(true) => Some(AutoIncrement::legacy()),
        _ => Some(parts.take(child.stored(token, values)?)),
    };
    child.end()?;
    parts.finish(result)
}

pub(super) fn read_sequence_restart<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    values: &Values<'_>,
) -> Result<Native<SequenceRestart>, InputError> {
    let mut child = input.content_child(token)?;
    let token = child.next()?;
    let mut parts = Parts::new(input.control);
    let result = match token {
        JsonToken::Null => SequenceRestart::Unchanged,
        JsonToken::Number(_) => SequenceRestart::With(crate::ast::stored_input::number(token)?),
        _ => parts.take(child.stored(token, values)?),
    };
    child.end()?;
    parts.finish(result)
}

mod explanation;
pub(super) use explanation::read_explain;
