//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical SQL equality encoding and borrowed-row hashing.

use std::hash::{BuildHasher, Hasher};

use smallvec::{Array, SmallVec};
use uqa_core::{memory::BudgetedVec, DecimalValue, TemporalValue, Value};
use uqa_storage::read_control::StorageReadControl;

mod output;
mod reservations;
mod traversal;
use output::{BudgetedOutput, KeyOutput};
pub(crate) use reservations::canonical_row_lock_keys;
use traversal::{Children, Frames};

use crate::{ExecError, ExecResult};

pub(super) const MICROS_PER_DAY: i128 = 86_400_000_000;

pub(crate) type EncodedKey = SmallVec<[u8; 64]>;

/// Hash a borrowed positional SQL row in its canonical equality domain.
///
/// This streams encoded components straight into the caller's hasher, so it
/// does not allocate or construct an intermediate byte key. Hash collisions
/// remain possible; callers must verify complete [`Value`] equality before
/// reusing an existing row or group.
pub fn hash_canonical_row<'a, S: BuildHasher>(
    build_hasher: &S,
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
) -> ExecResult<u64> {
    let count = values.len();
    let mut hasher = build_hasher.build_hasher();
    {
        let mut output = HasherOutput(&mut hasher);
        encode_len(count, &mut output)?;
        for value in values {
            if let Some(value) = value {
                encode_value(value, &mut output)?;
            } else {
                output.push_byte(0)?;
            }
        }
    }
    Ok(hasher.finish())
}

/// Pack exactly two text-or-NULL values of at most three bytes each into an injective integer key. `None` selects the general collision-safe encoder for every other row.
pub fn try_pack_compact_text_pair<'a>(
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
) -> Option<u64> {
    if values.len() != 2 {
        return None;
    }
    let mut values = values;
    let first = compact_text_component(values.next()?)?;
    let second = compact_text_component(values.next()?)?;
    Some(u64::from(first) << 32 | u64::from(second))
}

fn compact_text_component(value: Option<&Value>) -> Option<u32> {
    match value {
        None | Some(Value::Null) => Some(0),
        Some(Value::Str(value)) if value.len() <= 3 => {
            let mut packed = [0u8; 4];
            packed[0] = u8::try_from(value.len()).ok()? + 1;
            packed[1..][..value.len()].copy_from_slice(value.as_bytes());
            Some(u32::from_be_bytes(packed))
        }
        Some(_) => None,
    }
}

/// Encode positional values in Core's exact equality domain used by DISTINCT and spill-backed row-key state. SQL operator coercions must precede encoding. This is an execution key, not a versioned durable storage format.
pub fn canonical_row_key(values: &[Value]) -> ExecResult<Vec<u8>> {
    encode_key(values)
}

/// Encode borrowed positional values while charging output and normalization scratch to the original read allowance. No input value is cloned; the completed byte buffer retains its reservation.
pub fn canonical_row_key_budgeted<'a>(
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
    control: &StorageReadControl,
) -> ExecResult<BudgetedVec<u8>> {
    let mut output = BudgetedOutput::new(control);
    output.check()?;
    encode_len(values.len(), &mut output)?;
    for value in values {
        encode_value(value.unwrap_or(&Value::Null), &mut output)?;
    }
    output.check()?;
    Ok(output.into_values())
}

/// Collision-free binary key encoding. Numeric values deliberately share one
/// canonical domain so `1`, `1.0`, `DECIMAL '1'`, and `TRUE` retain the same
/// equality behavior as Core values. SQL operands must first receive their selected casts. Every structural value carries
/// lengths/counts, preventing concatenation and nested-container collisions.
pub(crate) fn encode_key(values: &[Value]) -> ExecResult<Vec<u8>> {
    encode_key_borrowed(values.iter().map(Some))
}

pub(super) fn encode_key_borrowed<'a>(
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
) -> ExecResult<Vec<u8>> {
    let estimated_capacity = encoded_key_capacity(values.len())?;
    let mut output = Vec::with_capacity(estimated_capacity);
    encode_len(values.len(), &mut output)?;
    for value in values {
        match value {
            Some(value) => encode_value(value, &mut output)?,
            None => encode_value(&Value::Null, &mut output)?,
        }
    }
    Ok(output)
}

/// Encode a join probe key directly from physical slots. Single- and
/// two-column numeric keys stay inline, and a NULL/missing component rejects
/// the SQL equality key without allocating or cloning a `Value`.
pub(crate) fn encode_non_null_key<'a>(
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
) -> ExecResult<Option<EncodedKey>> {
    let count = values.len();
    let mut output = EncodedKey::with_capacity(encoded_key_capacity(count)?);
    encode_len(count, &mut output)?;
    for value in values {
        let Some(value) = value else {
            return Ok(None);
        };
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        encode_value(value, &mut output)?;
    }
    Ok(Some(output))
}

fn encoded_key_capacity(values: usize) -> ExecResult<usize> {
    values
        .checked_mul(22)
        .and_then(|bytes| bytes.checked_add(8))
        .ok_or_else(|| encoding_error("DISTINCT key capacity overflow"))
}

impl KeyOutput for Vec<u8> {
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.push(value);
        Ok(())
    }

    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        self.extend_from_slice(values);
        Ok(())
    }
}

impl<A: Array<Item = u8>> KeyOutput for SmallVec<A> {
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.push(value);
        Ok(())
    }

    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        self.extend_from_slice(values);
        Ok(())
    }
}

struct HasherOutput<'a, H: Hasher>(&'a mut H);

impl<H: Hasher> KeyOutput for HasherOutput<'_, H> {
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.0.write_u8(value);
        Ok(())
    }

    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        self.0.write(values);
        Ok(())
    }
}

fn encode_value(value: &Value, output: &mut impl KeyOutput) -> ExecResult<()> {
    let control = output.control().cloned();
    let mut stack = Frames::new(control.as_ref());
    let mut current = Some(value);
    loop {
        output.check()?;
        if let Some(value) = current.take() {
            match value {
                Value::Null => output.push_byte(0)?,
                Value::Void => output.push_byte(13)?,
                Value::Bool(value) => {
                    output.extend_bytes(&[1, 0])?;
                    encode_bytes(if *value { b"1" } else { b"0" }, output)?;
                }
                Value::Int(value) => {
                    output.extend_bytes(&[1, 0])?;
                    let text = output::NumberText::new(*value)?;
                    encode_bytes(text.as_bytes(), output)?;
                }
                Value::Float(value) => encode_float_numeric(*value, output)?,
                Value::Decimal(value) => encode_decimal_numeric(value, output)?,
                Value::Str(value) => {
                    output.push_byte(2)?;
                    encode_bytes(value.as_bytes(), output)?;
                }
                Value::FixedChar(value) => {
                    output.push_byte(7)?;
                    encode_bytes(value.trim_end_matches(' ').as_bytes(), output)?;
                }
                Value::Bytes(value) => {
                    output.push_byte(3)?;
                    encode_bytes(value, output)?;
                }
                Value::Temporal(value) => encode_temporal(value, output)?,
                Value::Json(value) => {
                    output.push_byte(8)?;
                    encode_bytes(value.as_bytes(), output)?;
                }
                Value::JsonB(value) => output::encode_jsonb(value, output)?,
                Value::Array(array) => {
                    output.push_byte(12)?;
                    encode_len(array.lower_bounds().len(), output)?;
                    for lower_bound in array.lower_bounds() {
                        output.extend_bytes(&lower_bound.to_le_bytes())?;
                    }
                    encode_len(array.elements().len(), output)?;
                    if !array.elements().is_empty() {
                        stack.push(Children::Values(array.elements().iter()))?;
                    }
                }
                Value::List(values) | Value::Row(values) => {
                    output.push_byte(if matches!(value, Value::List(_)) {
                        5
                    } else {
                        10
                    })?;
                    encode_len(values.len(), output)?;
                    if !values.is_empty() {
                        stack.push(Children::Values(values.iter()))?;
                    }
                }
                Value::Record(fields) => {
                    output.push_byte(11)?;
                    encode_len(fields.len(), output)?;
                    if !fields.is_empty() {
                        stack.push(Children::Record(fields.iter()))?;
                    }
                }
                Value::Map(fields) => {
                    output.push_byte(6)?;
                    encode_len(fields.len(), output)?;
                    if !fields.is_empty() {
                        stack.push(Children::Map(fields.iter()))?;
                    }
                }
            }
        }
        while let Some(children) = stack.last_mut() {
            output.check()?;
            current = match children.next() {
                Some((name, value)) => {
                    if let Some(name) = name {
                        encode_bytes(name.as_bytes(), output)?;
                    }
                    Some(value)
                }
                None => None,
            };
            if current.is_some() {
                break;
            }
            stack.pop();
        }
        if current.is_none() {
            return output.check();
        }
    }
}

fn encode_decimal_numeric(value: &DecimalValue, output: &mut impl KeyOutput) -> ExecResult<()> {
    if value.is_nan() {
        output.extend_bytes(&[1, 1])?;
    } else if value.is_negative_infinity() {
        output.extend_bytes(&[1, 2])?;
    } else if value.is_positive_infinity() {
        output.extend_bytes(&[1, 3])?;
    } else {
        output.extend_bytes(&[1, 0])?;
        if let Some(control) = output.control() {
            let text = value
                .to_canonical_string_budgeted(control.memory(), control.cancellation())
                .map_err(output::resource_error)?;
            encode_bytes(text.as_bytes(), output)?;
        } else {
            encode_bytes(value.to_canonical_string().as_bytes(), output)?;
        }
    }
    Ok(())
}

fn encode_float_numeric(value: f64, output: &mut impl KeyOutput) -> ExecResult<()> {
    if value.is_nan() {
        // PostgreSQL groups all NaN values together for DISTINCT.
        output.extend_bytes(&[1, 1])?;
    } else if value == f64::NEG_INFINITY {
        output.extend_bytes(&[1, 2])?;
    } else if value == f64::INFINITY {
        output.extend_bytes(&[1, 3])?;
    } else if output.legacy_numeric_reservation() {
        // Keep the predecessor's opaque lock address alongside the exact key while older and newer processes share a database. This alias is never used for equality or ordering.
        if let Some(decimal) = DecimalValue::from_f64_lossy(value) {
            encode_decimal_numeric(&decimal, output)?;
        } else {
            output.extend_bytes(&[1, 4])?;
            output.extend_bytes(&value.to_bits().to_be_bytes())?;
        }
    } else if let Some(control) = output.control() {
        let production = uqa_core::memory::ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let decimal = DecimalValue::from_f64_exact_with_control(value, &production)
            .map_err(output::resource_error)?;
        encode_decimal_numeric(&decimal, output)?;
    } else {
        encode_decimal_numeric(&DecimalValue::from_f64_exact(value), output)?;
    }
    Ok(())
}

fn encode_temporal(value: &TemporalValue, output: &mut impl KeyOutput) -> ExecResult<()> {
    output.push_byte(4)?;
    match value {
        TemporalValue::Date { days } => {
            output.push_byte(0)?;
            output.extend_bytes(&days.to_be_bytes())?;
        }
        TemporalValue::Time { micros } => {
            output.push_byte(1)?;
            let normalized = i128::from(*micros).rem_euclid(MICROS_PER_DAY);
            output.extend_bytes(&normalized.to_be_bytes())?;
        }
        TemporalValue::TimeTz {
            micros,
            offset_minutes,
        } => {
            output.push_byte(2)?;
            let normalized = (i128::from(*micros) - i128::from(*offset_minutes) * 60_000_000)
                .rem_euclid(MICROS_PER_DAY);
            output.extend_bytes(&normalized.to_be_bytes())?;
        }
        TemporalValue::Timestamp { micros } => {
            output.push_byte(3)?;
            output.extend_bytes(&micros.to_be_bytes())?;
        }
        TemporalValue::TimestampTz { micros } => {
            output.push_byte(4)?;
            output.extend_bytes(&micros.to_be_bytes())?;
        }
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => {
            output.push_byte(5)?;
            let normalized = (i128::from(*months) * 30 + i128::from(*days)) * MICROS_PER_DAY
                + i128::from(*micros);
            output.extend_bytes(&normalized.to_be_bytes())?;
        }
    }
    Ok(())
}

fn encode_bytes(bytes: &[u8], output: &mut impl KeyOutput) -> ExecResult<()> {
    encode_len(bytes.len(), output)?;
    output.extend_bytes(bytes)?;
    Ok(())
}

fn encode_len(length: usize, output: &mut impl KeyOutput) -> ExecResult<()> {
    let length = u64::try_from(length)
        .map_err(|_| encoding_error("DISTINCT key component exceeds the binary format"))?;
    output.extend_bytes(&length.to_be_bytes())?;
    Ok(())
}

fn encoding_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}

#[cfg(test)]
mod tests;
