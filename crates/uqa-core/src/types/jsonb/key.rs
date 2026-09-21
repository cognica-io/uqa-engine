//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered JSONB keys reuse the native parser and comparator's normalized values.

use crate::{
    cancel::QueryCancelled,
    memory::{BudgetedVec, MemoryError},
    CancellationToken,
};

use super::{
    jsonb_key_storage_order, type_rank, workspace::Workspace, JsonNumber, JsonbParser, JsonbValue,
};

#[derive(Debug, thiserror::Error)]
pub enum JsonbKeyError {
    #[error("JSONB text has no native comparison representation")]
    InvalidJson,
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
}

/// Append an order-preserving key for text accepted by the native JSONB comparator. Parsing, sorting and output buffers share the output's allowance; failure restores its original length. Text outside the native parser's representation is reported explicitly instead of receiving an incompatible lexical key.
pub fn write_jsonb_comparison_key(
    text: &str,
    output: &mut BudgetedVec<u8>,
    cancellation: &CancellationToken,
) -> Result<(), JsonbKeyError> {
    let original = output.len();
    let result = (|| {
        let mut workspace = Workspace::bounded(output.budget(), cancellation);
        let value = JsonbParser::parse_with(text, &mut workspace)?;
        encode(&value, true, output, cancellation)?;
        cancellation.check()?;
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
    cancellation: &CancellationToken,
) -> Result<(), JsonbKeyError> {
    cancellation.check()?;
    if root && matches!(value, JsonbValue::Array(values) if values.is_empty()) {
        output.push(0)?;
        return Ok(());
    }
    output.push(type_rank(value) + 1)?;
    match value {
        JsonbValue::Null => {}
        JsonbValue::Bool(value) => output.push(u8::from(*value))?,
        JsonbValue::Number(value) => number(value, output, cancellation)?,
        JsonbValue::String(value) => text(value.as_bytes(), output, cancellation)?,
        JsonbValue::Array(values) => {
            length(values.len(), output)?;
            for value in values {
                encode(value, false, output, cancellation)?;
            }
        }
        JsonbValue::Object(values) => {
            length(values.len(), output)?;
            let mut ordered = BudgetedVec::new(output.budget());
            for value in values {
                cancellation.check()?;
                ordered.push(value)?;
            }
            ordered
                .sort_unstable_by(|left, right| jsonb_key_storage_order(&left.name, &right.name));
            for field in &*ordered {
                text(field.name.as_bytes(), output, cancellation)?;
                encode(&field.value, false, output, cancellation)?;
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
    cancellation: &CancellationToken,
) -> Result<(), JsonbKeyError> {
    for chunk in value.chunks(4096) {
        cancellation.check()?;
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
    cancellation: &CancellationToken,
) -> Result<(), JsonbKeyError> {
    output.push(u8::from(!value.negative))?;
    let mask = if value.negative { 255 } else { 0 };
    let mut rank = ((value.integer_digits() as u128) ^ (1_u128 << 127)).to_be_bytes();
    for byte in &mut rank {
        *byte ^= mask;
    }
    output.extend_from_slice(&rank)?;
    for chunk in value.digits.chunks(4096) {
        cancellation.check()?;
        for &byte in chunk {
            output.push(byte ^ mask)?;
        }
    }
    output.push(mask)?;
    Ok(())
}
