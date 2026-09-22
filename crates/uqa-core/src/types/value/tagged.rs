//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tagged value recognition validates borrowed shapes before moving decoded buffers.

use super::{ArrayValue, BTreeMap, DecimalValue, TemporalValue, Value};

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
pub(super) fn value_from_tagged_map(mut map: BTreeMap<String, Value>) -> Result<Value, String> {
    let Some(Value::Str(tag)) = map.get("$uqa_type") else {
        return Ok(Value::Map(map));
    };
    if let Some(value) = tagged_temporal_value(tag, &map) {
        return Ok(Value::Temporal(value));
    }
    match tag.as_str() {
        "void" if map.len() == 1 => return Ok(Value::Void),
        "decimal" => {
            if let Some(Value::Str(text)) = map.get("value") {
                if let Some(value) = DecimalValue::parse(text) {
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
                if let Some(bytes) = decode_hex_bytes(hex)? {
                    return Ok(Value::Bytes(bytes));
                }
            }
        }
        "array" if map.len() == 3 => {
            let (Some(Value::List(bounds)), Some(Value::List(values))) =
                (map.get("lower_bounds"), map.get("values"))
            else {
                return Ok(Value::Map(map));
            };
            let bounds = bounds
                .iter()
                .map(|value| match value {
                    Value::Int(value) => i32::try_from(*value).ok(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            if let Some(bounds) = bounds {
                if let Some(dimensions) = ArrayValue::decoded_shape(values) {
                    if dimensions.len() == bounds.len() {
                        let values = take_list(&mut map, "values");
                        return Ok(Value::Array(ArrayValue::from_decoded_parts(
                            values, dimensions, bounds,
                        )));
                    }
                }
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
            if !encoded_fields.iter().all(|encoded| {
                matches!(encoded, Value::List(pair) if matches!(pair.as_slice(), [Value::Str(_), _]))
            }) {
                return Ok(Value::Map(map));
            }
            let mut fields = Vec::new();
            fields
                .try_reserve_exact(encoded_fields.len())
                .map_err(|error| format!("cannot allocate decoded record fields: {error}"))?;
            for encoded in take_list(&mut map, "fields") {
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

fn take_list(map: &mut BTreeMap<String, Value>, key: &str) -> Vec<Value> {
    let Some(Value::List(values)) = map.remove(key) else {
        unreachable!("list tag was validated");
    };
    values
}

fn decode_hex_bytes(hex: &str) -> Result<Option<Vec<u8>>, String> {
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
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(encoded.len() / 2)
        .map_err(|error| format!("cannot allocate decoded byte value: {error}"))?;
    for pair in encoded.chunks_exact(2) {
        let (Some(high), Some(low)) = (nibble(pair[0]), nibble(pair[1])) else {
            return Ok(None);
        };
        bytes.push((high << 4) | low);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
