//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical JSONB equality bytes share one encoder for owned and controlled output.

use super::{type_rank, workspace::Workspace, JsonbField, JsonbKeyError, JsonbParser, JsonbValue};
use crate::{
    memory::{BudgetedVec, Produced, ProductionControl},
    CancellationToken, ValueRetentionError,
};

/// Return a semantic equality key for validated `jsonb` text.
#[must_use]
pub fn jsonb_equality_key(text: &str) -> Option<Vec<u8>> {
    jsonb_equality_key_with_control(text, &ProductionControl::uncontrolled())
        .ok()??
        .into_uncontrolled()
        .ok()
}

/// Produce the native semantic equality representation while admitting parsing, traversal and output to the caller's allowance. Invalid or unrepresentable JSON retains the ordinary API's `None` result; resource failures remain typed errors.
pub fn jsonb_equality_key_with_control(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<u8>>>, ValueRetentionError> {
    control.check()?;
    let result = (|| -> Result<Produced<Vec<u8>>, JsonbKeyError> {
        let mut workspace = Workspace::with_control(control);
        let value = JsonbParser::parse_with(text, &mut workspace)?;
        let (output, memory) = if let Some(budget) = control.budget() {
            let mut output = BudgetedVec::new(budget);
            encode(&value, &mut output, &mut workspace)?;
            let (output, memory) = output.into_parts();
            (output, Some(memory))
        } else {
            let mut output = Vec::new();
            encode(&value, &mut output, &mut workspace)?;
            (output, None)
        };
        // This transfer cannot allocate, and keeps the destination alive under its lease even if the final cancellation check fails.
        control.finish(output, memory).map_err(|error| match error {
            ValueRetentionError::Memory(error) => JsonbKeyError::Memory(error),
            ValueRetentionError::Cancelled(error) => JsonbKeyError::Cancelled(error),
        })
    })();
    match result {
        Ok(value) => Ok(Some(value)),
        Err(JsonbKeyError::InvalidJson) => Ok(None),
        Err(JsonbKeyError::Memory(error)) => Err(error.into()),
        Err(JsonbKeyError::Cancelled(error)) => Err(error.into()),
    }
}

/// Append the existing canonical equality representation while charging parsing, traversal and output to one allowance. Failure restores the output's original length.
pub fn write_jsonb_equality_key(
    text: &str,
    output: &mut BudgetedVec<u8>,
    cancellation: &CancellationToken,
) -> Result<(), JsonbKeyError> {
    let original = output.len();
    let result = (|| {
        let mut workspace = Workspace::bounded(output.budget(), cancellation);
        let value = JsonbParser::parse_with(text, &mut workspace)?;
        encode(&value, output, &mut workspace)
    })();
    if result.is_err() {
        output.truncate(original);
    }
    result
}

trait Output {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), JsonbKeyError>;
}

impl Output for Vec<u8> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), JsonbKeyError> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

impl Output for BudgetedVec<u8> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), JsonbKeyError> {
        self.extend_from_slice(bytes)?;
        Ok(())
    }
}

enum Children<'a> {
    Array(std::slice::Iter<'a, JsonbValue>),
    Object(std::slice::Iter<'a, JsonbField>),
}

fn encode(
    value: &JsonbValue,
    output: &mut impl Output,
    workspace: &mut Workspace<'_>,
) -> Result<(), JsonbKeyError> {
    let mut stack = workspace.buffer();
    let mut current = Some(value);
    loop {
        workspace.check()?;
        if let Some(value) = current.take() {
            output.bytes(&[type_rank(value)])?;
            match value {
                JsonbValue::Null => {}
                JsonbValue::Bool(value) => output.bytes(&[u8::from(*value)])?,
                JsonbValue::Number(value) => {
                    output.bytes(&[u8::from(value.negative)])?;
                    output.bytes(&value.power.to_be_bytes())?;
                    bytes(&value.digits, output, workspace)?;
                }
                JsonbValue::String(value) => bytes(value.as_bytes(), output, workspace)?,
                JsonbValue::Array(values) => {
                    length(values.len(), output)?;
                    if !values.is_empty() {
                        stack.push(Children::Array(values.iter()))?;
                    }
                }
                JsonbValue::Object(fields) => {
                    length(fields.len(), output)?;
                    if !fields.is_empty() {
                        stack.push(Children::Object(fields.iter()))?;
                    }
                }
            }
        }
        while let Some(children) = stack.last_mut() {
            workspace.check()?;
            current = match children {
                Children::Array(values) => values.next(),
                Children::Object(fields) => match fields.next() {
                    Some(field) => {
                        bytes(field.name.as_bytes(), output, workspace)?;
                        Some(&field.value)
                    }
                    None => None,
                },
            };
            if current.is_some() {
                break;
            }
            stack.pop();
        }
        if current.is_none() {
            return workspace.check();
        }
    }
}

fn length(length: usize, output: &mut impl Output) -> Result<(), JsonbKeyError> {
    let length = u64::try_from(length).map_err(|_| crate::memory::MemoryError::SizeOverflow)?;
    output.bytes(&length.to_be_bytes())
}

fn bytes(
    bytes: &[u8],
    output: &mut impl Output,
    workspace: &Workspace<'_>,
) -> Result<(), JsonbKeyError> {
    length(bytes.len(), output)?;
    for chunk in bytes.chunks(4096) {
        workspace.check()?;
        output.bytes(chunk)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
