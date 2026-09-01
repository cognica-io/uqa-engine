//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical SQL equality encoding and borrowed-row hashing.

use std::hash::{BuildHasher, Hasher};

use smallvec::{Array, SmallVec};
use uqa_core::{DecimalValue, TemporalValue, Value};

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
                output.push_byte(0);
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

/// Encode positional SQL values in the exact equality domain used by DISTINCT and spill-backed row-key state. Callers that need an external exact index can persist this representation without relying on `Value`'s serialization format.
pub fn canonical_row_key(values: &[Value]) -> ExecResult<Vec<u8>> {
    encode_key(values)
}

/// Collision-free binary key encoding. Numeric values deliberately share one
/// canonical domain so `1`, `1.0`, `DECIMAL '1'`, and `TRUE` retain the same
/// equality behavior as UQA's SQL comparisons. Every structural value carries
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

trait KeyOutput {
    fn push_byte(&mut self, value: u8);
    fn extend_bytes(&mut self, values: &[u8]);
}

impl KeyOutput for Vec<u8> {
    fn push_byte(&mut self, value: u8) {
        self.push(value);
    }

    fn extend_bytes(&mut self, values: &[u8]) {
        self.extend_from_slice(values);
    }
}

impl<A: Array<Item = u8>> KeyOutput for SmallVec<A> {
    fn push_byte(&mut self, value: u8) {
        self.push(value);
    }

    fn extend_bytes(&mut self, values: &[u8]) {
        self.extend_from_slice(values);
    }
}

struct HasherOutput<'a, H: Hasher>(&'a mut H);

impl<H: Hasher> KeyOutput for HasherOutput<'_, H> {
    fn push_byte(&mut self, value: u8) {
        self.0.write_u8(value);
    }

    fn extend_bytes(&mut self, values: &[u8]) {
        self.0.write(values);
    }
}

fn encode_value(value: &Value, output: &mut impl KeyOutput) -> ExecResult<()> {
    match value {
        Value::Null => output.push_byte(0),
        Value::Bool(value) => {
            encode_decimal_numeric(&DecimalValue::from_bool(*value), output)?;
        }
        Value::Int(value) => {
            encode_decimal_numeric(&DecimalValue::from_i64(*value), output)?;
        }
        Value::Float(value) => encode_float_numeric(*value, output)?,
        Value::Decimal(value) => encode_decimal_numeric(value, output)?,
        Value::Str(value) => {
            output.push_byte(2);
            encode_bytes(value.as_bytes(), output)?;
        }
        Value::FixedChar(value) => {
            output.push_byte(7);
            encode_bytes(value.trim_end_matches(' ').as_bytes(), output)?;
        }
        Value::Bytes(value) => {
            output.push_byte(3);
            encode_bytes(value, output)?;
        }
        Value::Temporal(value) => encode_temporal(value, output),
        Value::Json(value) => {
            output.push_byte(8);
            encode_bytes(value.as_bytes(), output)?;
        }
        Value::JsonB(value) => {
            output.push_byte(9);
            let canonical = uqa_core::jsonb_equality_key(value)
                .ok_or_else(|| ExecError::Other("stored JSONB value is not valid JSON".into()))?;
            encode_bytes(&canonical, output)?;
        }
        Value::Array(array) => {
            output.push_byte(12);
            encode_len(array.lower_bounds().len(), output)?;
            for lower_bound in array.lower_bounds() {
                output.extend_bytes(&lower_bound.to_le_bytes());
            }
            encode_len(array.elements().len(), output)?;
            for value in array.elements() {
                encode_value(value, output)?;
            }
        }
        Value::List(values) => {
            output.push_byte(5);
            encode_len(values.len(), output)?;
            for value in values {
                encode_value(value, output)?;
            }
        }
        Value::Row(values) => {
            output.push_byte(10);
            encode_len(values.len(), output)?;
            for value in values {
                encode_value(value, output)?;
            }
        }
        Value::Record(fields) => {
            output.push_byte(11);
            encode_len(fields.len(), output)?;
            for (_, value) in fields {
                encode_value(value, output)?;
            }
        }
        Value::Map(values) => {
            output.push_byte(6);
            encode_len(values.len(), output)?;
            for (name, value) in values {
                encode_bytes(name.as_bytes(), output)?;
                encode_value(value, output)?;
            }
        }
    }
    Ok(())
}

fn encode_decimal_numeric(value: &DecimalValue, output: &mut impl KeyOutput) -> ExecResult<()> {
    if value.is_nan() {
        output.extend_bytes(&[1, 1]);
    } else if value.is_negative_infinity() {
        output.extend_bytes(&[1, 2]);
    } else if value.is_positive_infinity() {
        output.extend_bytes(&[1, 3]);
    } else {
        output.extend_bytes(&[1, 0]);
        encode_bytes(value.to_canonical_string().as_bytes(), output)?;
    }
    Ok(())
}

fn encode_float_numeric(value: f64, output: &mut impl KeyOutput) -> ExecResult<()> {
    if value.is_nan() {
        // PostgreSQL groups all NaN values together for DISTINCT.
        output.extend_bytes(&[1, 1]);
    } else if value == f64::NEG_INFINITY {
        output.extend_bytes(&[1, 2]);
    } else if value == f64::INFINITY {
        output.extend_bytes(&[1, 3]);
    } else if let Some(decimal) = DecimalValue::from_f64_lossy(value) {
        encode_decimal_numeric(&decimal, output)?;
    } else {
        // Preserve a finite value that cannot enter PostgreSQL's NUMERIC
        // domain. Normalize signed zero before storing bits.
        output.extend_bytes(&[1, 4]);
        let normalized = if value == 0.0 { 0.0 } else { value };
        output.extend_bytes(&normalized.to_bits().to_be_bytes());
    }
    Ok(())
}

fn encode_temporal(value: &TemporalValue, output: &mut impl KeyOutput) {
    output.push_byte(4);
    match value {
        TemporalValue::Date { days } => {
            output.push_byte(0);
            output.extend_bytes(&days.to_be_bytes());
        }
        TemporalValue::Time { micros } => {
            output.push_byte(1);
            let normalized = i128::from(*micros).rem_euclid(MICROS_PER_DAY);
            output.extend_bytes(&normalized.to_be_bytes());
        }
        TemporalValue::TimeTz {
            micros,
            offset_minutes,
        } => {
            output.push_byte(2);
            let normalized = (i128::from(*micros) - i128::from(*offset_minutes) * 60_000_000)
                .rem_euclid(MICROS_PER_DAY);
            output.extend_bytes(&normalized.to_be_bytes());
        }
        TemporalValue::Timestamp { micros } => {
            output.push_byte(3);
            output.extend_bytes(&micros.to_be_bytes());
        }
        TemporalValue::TimestampTz { micros } => {
            output.push_byte(4);
            output.extend_bytes(&micros.to_be_bytes());
        }
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => {
            output.push_byte(5);
            let normalized = (i128::from(*months) * 30 + i128::from(*days)) * MICROS_PER_DAY
                + i128::from(*micros);
            output.extend_bytes(&normalized.to_be_bytes());
        }
    }
}

fn encode_bytes(bytes: &[u8], output: &mut impl KeyOutput) -> ExecResult<()> {
    encode_len(bytes.len(), output)?;
    output.extend_bytes(bytes);
    Ok(())
}

fn encode_len(length: usize, output: &mut impl KeyOutput) -> ExecResult<()> {
    let length = u64::try_from(length)
        .map_err(|_| encoding_error("DISTINCT key component exceeds the binary format"))?;
    output.extend_bytes(&length.to_be_bytes());
    Ok(())
}

fn encoding_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}
