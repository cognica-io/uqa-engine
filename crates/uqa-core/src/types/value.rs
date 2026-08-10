//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dynamic document values, serialization, and cross-numeric ordering.

use super::{
    BTreeMap, DecimalValue, Deserialize, Deserializer, Serialize, Serializer, TemporalValue,
};

/// Dynamic value type for document fields and posting payload extras.
///
/// Covers the JSON-like values the engine round-trips through a posting
/// list. Date and datetime variants land with the SQL type system.
#[derive(Debug, Clone, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A `PostgreSQL` blank-padded `CHARACTER(n)` value. The stored string
    /// includes its physical trailing spaces; SQL comparisons ignore them.
    FixedChar(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Float(value) => serializer.serialize_f64(*value),
            Self::Str(value) => serializer.serialize_str(value),
            Self::FixedChar(value) => {
                #[derive(Serialize)]
                struct TaggedFixedChar<'a> {
                    #[serde(rename = "$uqa_type")]
                    kind: &'static str,
                    value: &'a str,
                }

                TaggedFixedChar {
                    kind: "fixed_char",
                    value,
                }
                .serialize(serializer)
            }
            Self::Bytes(value) => {
                const DIGITS: &[u8; 16] = b"0123456789abcdef";

                #[derive(Serialize)]
                struct TaggedBytes<'a> {
                    #[serde(rename = "$uqa_type")]
                    kind: &'static str,
                    hex: &'a str,
                }

                let capacity = value.len().checked_mul(2).ok_or_else(|| {
                    <S::Error as serde::ser::Error>::custom(
                        "byte value hex representation exceeds the addressable range",
                    )
                })?;
                let mut hex = String::new();
                hex.try_reserve_exact(capacity).map_err(|error| {
                    <S::Error as serde::ser::Error>::custom(format!(
                        "cannot allocate byte value hex representation: {error}"
                    ))
                })?;
                for byte in value {
                    hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
                    hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
                }
                TaggedBytes {
                    kind: "bytes",
                    hex: &hex,
                }
                .serialize(serializer)
            }
            Self::Temporal(value) => value.serialize(serializer),
            Self::Decimal(value) => value.serialize(serializer),
            Self::List(value) => value.serialize(serializer),
            Self::Map(value) => value.serialize(serializer),
        }
    }
}

/// Reconstruct the value a `$uqa_type`-tagged map encodes, or `None`
/// when the map does not match any tagged encoding and must stay a
/// plain [`Value::Map`].
///
/// Temporal variants mirror the `deny_unknown_fields` internally-tagged
/// derive on [`TemporalValue`]: the field set must match exactly and
/// every field must be an in-range integer. The decimal encoding
/// mirrors the tolerant tagged struct in [`DecimalValue`]'s
/// `Deserialize`: extra fields are ignored.
fn value_from_tagged_map(
    tag: &str,
    map: &BTreeMap<String, Value>,
) -> Result<Option<Value>, String> {
    fn int_field<T: TryFrom<i64>>(map: &BTreeMap<String, Value>, key: &str) -> Option<T> {
        match map.get(key)? {
            Value::Int(number) => T::try_from(*number).ok(),
            _ => None,
        }
    }

    let temporal = match tag {
        "date" if map.len() == 2 => {
            let Some(days) = int_field(map, "days") else {
                return Ok(None);
            };
            TemporalValue::Date { days }
        }
        "time" if map.len() == 2 => {
            let Some(micros) = int_field(map, "micros") else {
                return Ok(None);
            };
            TemporalValue::Time { micros }
        }
        "time_tz" if map.len() == 3 => {
            let (Some(micros), Some(offset_minutes)) =
                (int_field(map, "micros"), int_field(map, "offset_minutes"))
            else {
                return Ok(None);
            };
            TemporalValue::TimeTz {
                micros,
                offset_minutes,
            }
        }
        "timestamp" if map.len() == 2 => {
            let Some(micros) = int_field(map, "micros") else {
                return Ok(None);
            };
            TemporalValue::Timestamp { micros }
        }
        "timestamp_tz" if map.len() == 2 => {
            let Some(micros) = int_field(map, "micros") else {
                return Ok(None);
            };
            TemporalValue::TimestampTz { micros }
        }
        "interval" if map.len() == 4 => {
            let (Some(months), Some(days), Some(micros)) = (
                int_field(map, "months"),
                int_field(map, "days"),
                int_field(map, "micros"),
            ) else {
                return Ok(None);
            };
            TemporalValue::Interval {
                months,
                days,
                micros,
            }
        }
        "decimal" => {
            let Some(Value::Str(text)) = map.get("value") else {
                return Ok(None);
            };
            return Ok(DecimalValue::parse(text).map(Value::Decimal));
        }
        "fixed_char" if map.len() == 2 => {
            let Some(Value::Str(text)) = map.get("value") else {
                return Ok(None);
            };
            return Ok(Some(Value::FixedChar(text.clone())));
        }
        "bytes" if map.len() == 2 => {
            let Some(Value::Str(hex)) = map.get("hex") else {
                return Ok(None);
            };
            return decode_hex_bytes(hex).map(|bytes| bytes.map(Value::Bytes));
        }
        _ => return Ok(None),
    };
    Ok(Some(Value::Temporal(temporal)))
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

/// Hand-written [`Deserialize`] for scalar JSON values, explicit tagged
/// byte/temporal/decimal values, ordinary arrays, and maps, without the untagged
/// machinery's per-variant trial errors. Untagged deserialization
/// buffers the input and formats a rejection error for every variant
/// that does not match; profiling showed that error construction alone
/// consuming a quarter of `SQLite` read time. The visitor dispatches on
/// the self-describing input directly instead.
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> serde::de::Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a UQA value")
            }

            fn visit_unit<E>(self) -> std::result::Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_none<E>(self) -> std::result::Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                Value::deserialize(deserializer)
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Value, E> {
                Ok(Value::Int(value))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Value, E> {
                // The untagged order tries Int(i64) before Float, so
                // only out-of-range magnitudes land on Float.
                Ok(i64::try_from(value).map_or(Value::Float(value as f64), Value::Int))
            }

            fn visit_f64<E>(self, value: f64) -> std::result::Result<Value, E> {
                Ok(Value::Float(value))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Value, E> {
                Ok(Value::Str(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Value, E> {
                Ok(Value::Str(value))
            }

            fn visit_bytes<E>(self, value: &[u8]) -> std::result::Result<Value, E> {
                Ok(Value::Bytes(value.to_vec()))
            }

            fn visit_byte_buf<E>(self, value: Vec<u8>) -> std::result::Result<Value, E> {
                Ok(Value::Bytes(value))
            }

            fn visit_seq<A>(self, mut access: A) -> std::result::Result<Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                // `SeqAccess::size_hint` is supplied by the input decoder and
                // is not trustworthy enough to hand directly to an infallible
                // allocation. Reserve a bounded useful prefix, then make every
                // subsequent growth fallible as elements actually arrive.
                let initial = access.size_hint().unwrap_or(0).min(4_096);
                let mut items: Vec<Value> = Vec::new();
                items.try_reserve_exact(initial).map_err(|error| {
                    <A::Error as serde::de::Error>::custom(format!(
                        "cannot allocate UQA value sequence: {error}"
                    ))
                })?;
                while let Some(item) = access.next_element::<Value>()? {
                    if items.len() == items.capacity() {
                        items.try_reserve(1).map_err(|error| {
                            <A::Error as serde::de::Error>::custom(format!(
                                "cannot grow UQA value sequence: {error}"
                            ))
                        })?;
                    }
                    items.push(item);
                }
                Ok(Value::List(items))
            }

            fn visit_map<A>(self, mut access: A) -> std::result::Result<Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, Value>()? {
                    map.insert(key, value);
                }
                if let Some(Value::Str(tag)) = map.get("$uqa_type") {
                    if let Some(value) = value_from_tagged_map(tag, &map)
                        .map_err(<A::Error as serde::de::Error>::custom)?
                    {
                        return Ok(value);
                    }
                }
                Ok(Value::Map(map))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

fn compare_floats(left: f64, right: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if left == right {
        return Ordering::Equal;
    }
    if left.is_nan() {
        return if right.is_nan() {
            Ordering::Equal
        } else {
            Ordering::Greater
        };
    }
    if right.is_nan() || left < right {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn compare_integer_float(integer: i64, float: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    const I64_LOWER_INCLUSIVE: f64 = -9_223_372_036_854_775_808.0;

    if float.is_nan() || float >= I64_UPPER_EXCLUSIVE {
        return Ordering::Less;
    }
    if float < I64_LOWER_INCLUSIVE {
        return Ordering::Greater;
    }
    let truncated = float.trunc() as i64;
    match integer.cmp(&truncated) {
        Ordering::Equal if float > truncated as f64 => Ordering::Less,
        Ordering::Equal if float < truncated as f64 => Ordering::Greater,
        ordering => ordering,
    }
}

fn compare_float_decimal(float: f64, decimal: &DecimalValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if float.is_nan() || float == f64::INFINITY {
        return Ordering::Greater;
    }
    if float == f64::NEG_INFINITY {
        return Ordering::Less;
    }
    if let Some(float_decimal) = DecimalValue::from_f64_lossy(float) {
        return float_decimal.cmp(decimal);
    }

    // A finite f64 that rust_decimal cannot represent is either outside its
    // magnitude or below its scale. Compare such subnormal/supernormal values
    // against zero and the decimal sign without manufacturing an equality.
    let decimal_vs_zero = decimal.cmp(&DecimalValue::from_i64(0));
    if float > 0.0 {
        if float > 1.0 {
            Ordering::Greater
        } else if decimal_vs_zero == Ordering::Greater {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if float < -1.0 {
        Ordering::Less
    } else if decimal_vs_zero == Ordering::Less {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

// `Value` carries `f64`, so equality and ordering are implemented together.
// NaN compares equal to NaN and greater than finite values, signed zeroes are
// equal, and cross-numeric variants use numeric rather than discriminant order.
// This keeps `Eq`/`Ord` consistent for BTree keys used by joins and DISTINCT.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => compare_floats(*a, *b),
            (Value::Decimal(a), Value::Decimal(b)) => a.cmp(b),
            // Numeric cross-type compare: Int / Float / Bool all coerce
            // to f64 so SQL `WHERE price > 15` (Float vs Int literal)
            // and `WHERE flag > 0` line up with PostgreSQL semantics
            // instead of falling through to the discriminant order.
            (Value::Int(a), Value::Float(b)) => compare_integer_float(*a, *b),
            (Value::Float(a), Value::Int(b)) => compare_integer_float(*b, *a).reverse(),
            (Value::Int(a), Value::Decimal(b)) => DecimalValue::from_i64(*a).cmp(b),
            (Value::Decimal(a), Value::Int(b)) => a.cmp(&DecimalValue::from_i64(*b)),
            (Value::Float(a), Value::Decimal(b)) => compare_float_decimal(*a, b),
            (Value::Decimal(a), Value::Float(b)) => compare_float_decimal(*b, a).reverse(),
            (Value::Bool(a), Value::Int(b)) => i64::from(*a).cmp(b),
            (Value::Int(a), Value::Bool(b)) => a.cmp(&i64::from(*b)),
            (Value::Bool(a), Value::Float(b)) => compare_integer_float(i64::from(*a), *b),
            (Value::Float(a), Value::Bool(b)) => compare_integer_float(i64::from(*b), *a).reverse(),
            (Value::Bool(a), Value::Decimal(b)) => DecimalValue::from_bool(*a).cmp(b),
            (Value::Decimal(a), Value::Bool(b)) => a.cmp(&DecimalValue::from_bool(*b)),
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            (Value::FixedChar(a), Value::FixedChar(b)) => {
                fixed_char_text(a).cmp(fixed_char_text(b))
            }
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            (Value::Temporal(a), Value::Temporal(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.cmp(b),
            _ => discriminant(self).cmp(&discriminant(other)),
        }
    }
}

fn discriminant(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Decimal(_) => 1,
        Value::Str(_) => 2,
        Value::FixedChar(_) => 3,
        Value::Bytes(_) => 4,
        Value::Temporal(_) => 5,
        Value::List(_) => 6,
        Value::Map(_) => 7,
    }
}

fn fixed_char_text(value: &str) -> &str {
    value.trim_end_matches(' ')
}
