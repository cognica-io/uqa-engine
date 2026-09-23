//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered JSONB keys reuse the native parser and comparator's normalized values.

use crate::{
    cancel::QueryCancelled,
    memory::{BudgetedVec, MemoryError, ProductionControl},
    CancellationToken,
};

use super::{type_rank, workspace::Workspace, JsonNumber, JsonbParser, JsonbValue};

#[derive(Debug, thiserror::Error)]
pub enum JsonbKeyError {
    #[error("JSONB text has no native comparison representation")]
    InvalidJson,
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
}

impl From<crate::json::JsonReadError> for JsonbKeyError {
    fn from(error: crate::json::JsonReadError) -> Self {
        match error {
            crate::json::JsonReadError::InvalidJson => Self::InvalidJson,
            crate::json::JsonReadError::Memory(error) => Self::Memory(error),
            crate::json::JsonReadError::Cancelled(error) => Self::Cancelled(error),
        }
    }
}

/// Append an order-preserving key for text accepted by the native JSONB comparator. Parsing, sorting and output buffers share the output's allowance; failure restores its original length. Text outside the native parser's representation is reported explicitly instead of receiving an incompatible lexical key.
pub fn write_jsonb_comparison_key(
    text: &str,
    output: &mut BudgetedVec<u8>,
    cancellation: &CancellationToken,
) -> Result<(), JsonbKeyError> {
    let budget = output.budget().clone();
    let control = ProductionControl::new(&budget, cancellation, cancellation);
    write_with_control(text, output, &control)
}

pub(super) fn write_with_control(
    text: &str,
    output: &mut BudgetedVec<u8>,
    control: &ProductionControl<'_>,
) -> Result<(), JsonbKeyError> {
    let original = output.len();
    let result = (|| {
        let mut workspace = Workspace::with_control(control);
        let value = JsonbParser::parse_with(text, &mut workspace)?;
        encode(&value, true, output, control)?;
        control.check_cancellation()?;
        Ok(())
    })();
    if result.is_err() {
        output.truncate(original);
    }
    result
}

fn encode(
    value: &JsonbValue,
    root: bool,
    output: &mut BudgetedVec<u8>,
    control: &ProductionControl<'_>,
) -> Result<(), JsonbKeyError> {
    control.check_cancellation()?;
    if root && matches!(value, JsonbValue::Array(values) if values.is_empty()) {
        output.push(0)?;
        return Ok(());
    }
    output.push(type_rank(value) + 1)?;
    match value {
        JsonbValue::Null => {}
        JsonbValue::Bool(value) => output.push(u8::from(*value))?,
        JsonbValue::Number(value) => number(value, output, control)?,
        JsonbValue::String(value) => text(value.as_bytes(), output, control)?,
        JsonbValue::Array(values) => {
            length(values.len(), output)?;
            for value in values {
                encode(value, false, output, control)?;
            }
        }
        JsonbValue::Object(values) => {
            length(values.len(), output)?;
            let mut ordered = BudgetedVec::new(output.budget());
            for value in values {
                control.check_cancellation()?;
                ordered.push(value)?;
            }
            crate::ordering::sort_by_with_control(
                &mut ordered,
                &mut || control.check_cancellation().map_err(JsonbKeyError::from),
                |left, right, _| {
                    let length = left.name.len().cmp(&right.name.len());
                    if !length.is_eq() {
                        return Ok(length);
                    }
                    super::super::value::comparison_control::compare_bytes(
                        left.name.as_bytes(),
                        right.name.as_bytes(),
                        control,
                    )
                    .map_err(|error| match error {
                        crate::ValueRetentionError::Memory(error) => JsonbKeyError::Memory(error),
                        crate::ValueRetentionError::Cancelled(error) => {
                            JsonbKeyError::Cancelled(error)
                        }
                    })
                },
            )?;
            for field in &*ordered {
                text(field.name.as_bytes(), output, control)?;
                encode(&field.value, false, output, control)?;
            }
        }
    }
    Ok(())
}

fn length(length: usize, output: &mut BudgetedVec<u8>) -> Result<(), JsonbKeyError> {
    let length = u64::try_from(length).map_err(|_| MemoryError::SizeOverflow)?;
    output.extend_from_slice(&length.to_be_bytes())?;
    Ok(())
}

fn text(
    value: &[u8],
    output: &mut BudgetedVec<u8>,
    control: &ProductionControl<'_>,
) -> Result<(), JsonbKeyError> {
    for chunk in value.chunks(4096) {
        control.check_cancellation()?;
        for &byte in chunk {
            if byte == 0 {
                output.extend_from_slice(&[0, 255])?;
            } else {
                output.push(byte)?;
            }
        }
    }
    output.extend_from_slice(&[0, 0])?;
    Ok(())
}

fn number(
    value: &JsonNumber,
    output: &mut BudgetedVec<u8>,
    control: &ProductionControl<'_>,
) -> Result<(), JsonbKeyError> {
    output.push(u8::from(!value.negative))?;
    let mask = if value.negative { 255 } else { 0 };
    let mut rank = ((value.integer_digits() as u128) ^ (1_u128 << 127)).to_be_bytes();
    for byte in &mut rank {
        *byte ^= mask;
    }
    output.extend_from_slice(&rank)?;
    for chunk in value.digits.chunks(4096) {
        control.check_cancellation()?;
        for &byte in chunk {
            output.push(byte ^ mask)?;
        }
    }
    output.push(mask)?;
    Ok(())
}
