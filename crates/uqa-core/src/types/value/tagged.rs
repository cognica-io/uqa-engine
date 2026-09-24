//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tagged value recognition validates borrowed shapes before moving decoded buffers.

use super::{ArrayValue, BTreeMap, TemporalValue, Value};
use crate::{
    memory::Budgeted, CancellationToken, LegacyVectorKind, LegacyVectorValue, ValueRetentionError,
};

mod allocation;
use allocation::Workspace;

fn int_field<T: TryFrom<i64>>(map: &BTreeMap<String, Value>, key: &str) -> Option<T> {
    match map.get(key)? {
        Value::Int(number) => T::try_from(*number).ok(),
        _ => None,
    }
}

fn tagged_temporal_value(tag: &str, map: &BTreeMap<String, Value>) -> Option<TemporalValue> {
    match tag {
        "date" if map.len() == 2 => Some(TemporalValue::Date {
            days: int_field(map, "days")?,
        }),
        "time" if map.len() == 2 => Some(TemporalValue::Time {
            micros: int_field(map, "micros")?,
        }),
        "time_tz" if map.len() == 3 => Some(TemporalValue::TimeTz {
            micros: int_field(map, "micros")?,
            offset_minutes: int_field(map, "offset_minutes")?,
        }),
        "timestamp" if map.len() == 2 => Some(TemporalValue::Timestamp {
            micros: int_field(map, "micros")?,
        }),
        "timestamp_tz" if map.len() == 2 => Some(TemporalValue::TimestampTz {
            micros: int_field(map, "micros")?,
        }),
        "interval" if map.len() == 4 => Some(TemporalValue::Interval {
            months: int_field(map, "months")?,
            days: int_field(map, "days")?,
            micros: int_field(map, "micros")?,
        }),
        _ => None,
    }
}

/// Consume a recognized tagged map without cloning its decoded payload. Invalid tags retain every original field and value.
pub(super) fn value_from_tagged_map(map: BTreeMap<String, Value>) -> Result<Value, String> {
    convert(map, &mut Workspace::unbounded()).map_err(|error| error.to_string())
}

/// The input lease includes live map entries, key capacities and nested value payloads. Reserve new tag representations before allocation, then retain only the resulting value's payload.
pub(super) fn value_from_tagged_map_budgeted(
    map: Budgeted<BTreeMap<String, Value>>,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Value>, ValueRetentionError> {
    let (map, memory) = map.into_parts();
    let mut workspace = Workspace::bounded(memory, cancellation);
    let value = convert(map, &mut workspace)?;
    let retained = workspace.retained(&value)?;
    Ok(Budgeted::new(value, retained))
}

fn convert(
    mut map: BTreeMap<String, Value>,
    workspace: &mut Workspace<'_>,
) -> Result<Value, ValueRetentionError> {
    workspace.check()?;
    let Some(Value::Str(tag)) = map.get("$uqa_type") else {
        return Ok(Value::Map(map));
    };
    if let Some(value) = tagged_temporal_value(tag, &map) {
        return Ok(Value::Temporal(value));
    }
    match tag.as_str() {
        "void" if map.len() == 1 => return Ok(Value::Void),
        "float_bits" if map.len() == 2 => {
            if let Some(Value::Str(hex)) = map.get("hex") {
                if let Some(value) = super::nonfinite::decode(hex) {
                    return Ok(Value::Float(value));
                }
            }
        }
        "decimal" => {
            if let Some(Value::Str(text)) = map.get("value") {
                if let Some(value) = workspace.decimal(text)? {
                    return Ok(Value::Decimal(value));
                }
            }
        }
        "fixed_char" | "json" | "jsonb" if map.len() == 2 => {
            let wrap = match tag.as_str() {
                "fixed_char" => Value::FixedChar,
                "json" => Value::Json,
                _ => Value::JsonB,
            };
            if matches!(map.get("value"), Some(Value::Str(_))) {
                let Some(Value::Str(text)) = map.remove("value") else {
                    unreachable!("text tag was validated");
                };
                return Ok(wrap(text));
            }
        }
        "bytes" if map.len() == 2 => {
            if let Some(Value::Str(hex)) = map.get("hex") {
                if let Some(bytes) = decode_hex_bytes(hex, workspace)? {
                    return Ok(Value::Bytes(bytes));
                }
            }
        }
        "array" if map.len() == 3 => {
            if let Some(array) = decoded_array(&mut map, workspace)? {
                return Ok(Value::Array(array));
            }
        }
        "int2vector" | "oidvector" if map.len() == 2 || map.len() == 3 => {
            let kind = if tag == "int2vector" {
                LegacyVectorKind::SmallInteger
            } else {
                LegacyVectorKind::Oid
            };
            if let Some(vector) = decoded_legacy_vector(&mut map, kind, workspace)? {
                return Ok(Value::LegacyVector(vector));
            }
        }
        "row" if map.len() == 2 => {
            if matches!(map.get("values"), Some(Value::List(_))) {
                return Ok(Value::Row(take_list(&mut map, "values")));
            }
        }
        "record" if map.len() == 2 => {
            let Some(Value::List(encoded_fields)) = map.get("fields") else {
                return Ok(Value::Map(map));
            };
            for encoded in encoded_fields {
                workspace.check()?;
                if !matches!(encoded, Value::List(pair) if matches!(pair.as_slice(), [Value::Str(_), _]))
                {
                    return Ok(Value::Map(map));
                }
            }
            let mut fields = workspace.vector(encoded_fields.len())?;
            for encoded in take_list(&mut map, "fields") {
                workspace.check()?;
                let Value::List(mut pair) = encoded else {
                    unreachable!("record pair was validated");
                };
                let value = pair.pop().expect("validated record value");
                let Some(Value::Str(name)) = pair.pop() else {
                    unreachable!("record name was validated");
                };
                fields.push((name, value));
            }
            return Ok(Value::Record(fields));
        }
        _ => {}
    }
    Ok(Value::Map(map))
}

fn decoded_legacy_vector(
    map: &mut BTreeMap<String, Value>,
    kind: LegacyVectorKind,
    workspace: &mut Workspace<'_>,
) -> Result<Option<LegacyVectorValue>, ValueRetentionError> {
    let Some(Value::List(values)) = map.get("values") else {
        return Ok(None);
    };
    for value in values {
        workspace.check()?;
        if !kind.accepts(value) {
            return Ok(None);
        }
    }
    if map.contains_key("lower_bounds") {
        let Some(Value::List(bounds)) = map.get("lower_bounds") else {
            return Ok(None);
        };
        if bounds.len() > 1 {
            return Ok(None);
        }
        return Ok(decoded_array(map, workspace)?
            .map(|array| LegacyVectorValue::from_validated_array(kind, array)));
    }
    if map.len() != 2 {
        return Ok(None);
    }
    let mut dimensions = workspace.vector(1)?;
    dimensions.push(values.len());
    let mut bounds = workspace.vector(1)?;
    bounds.push(0);
    workspace.reserve(ArrayValue::decoded_header_bytes())?;
    let array = ArrayValue::from_decoded_parts(take_list(map, "values"), dimensions, bounds);
    Ok(Some(LegacyVectorValue::from_validated_array(kind, array)))
}

fn decoded_array(
    map: &mut BTreeMap<String, Value>,
    workspace: &mut Workspace<'_>,
) -> Result<Option<ArrayValue>, ValueRetentionError> {
    let (Some(Value::List(bounds)), Some(Value::List(values))) =
        (map.get("lower_bounds"), map.get("values"))
    else {
        return Ok(None);
    };
    for bound in bounds {
        workspace.check()?;
        if !matches!(bound, Value::Int(value) if i32::try_from(*value).is_ok()) {
            return Ok(None);
        }
    }
    let Some(dimensions) = workspace.shape(values, bounds.len())? else {
        return Ok(None);
    };
    if dimensions.len() != bounds.len() {
        return Ok(None);
    }
    let mut decoded_bounds = workspace.vector(bounds.len())?;
    for bound in bounds {
        workspace.check()?;
        let Value::Int(bound) = bound else {
            unreachable!("array lower bound was validated");
        };
        decoded_bounds.push(i32::try_from(*bound).expect("validated lower bound"));
    }
    workspace.reserve(ArrayValue::decoded_header_bytes())?;
    let values = take_list(map, "values");
    Ok(Some(ArrayValue::from_decoded_parts(
        values,
        dimensions,
        decoded_bounds,
    )))
}

fn take_list(map: &mut BTreeMap<String, Value>, key: &str) -> Vec<Value> {
    let Some(Value::List(values)) = map.remove(key) else {
        unreachable!("list tag was validated");
    };
    values
}

fn decode_hex_bytes(
    hex: &str,
    workspace: &mut Workspace<'_>,
) -> Result<Option<Vec<u8>>, ValueRetentionError> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let encoded = hex.as_bytes();
    if !encoded.len().is_multiple_of(2) {
        return Ok(None);
    }
    for chunk in encoded.chunks(4096) {
        workspace.check()?;
        if chunk.iter().any(|byte| nibble(*byte).is_none()) {
            return Ok(None);
        }
    }
    let mut bytes = workspace.vector(encoded.len() / 2)?;
    for (index, pair) in encoded.chunks_exact(2).enumerate() {
        if index.is_multiple_of(4096) {
            workspace.check()?;
        }
        let high = nibble(pair[0]).expect("validated hexadecimal digit");
        let low = nibble(pair[1]).expect("validated hexadecimal digit");
        bytes.push((high << 4) | low);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod controlled_tests;
