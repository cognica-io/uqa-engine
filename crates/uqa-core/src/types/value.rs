//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dynamic document values, serialization, and cross-numeric ordering.

use super::{
    jsonb::compare_jsonb_text, ArrayValue, BTreeMap, DecimalValue, Deserialize, Deserializer,
    Serialize, Serializer, TemporalValue,
};

/// Dynamic value type for document fields and posting payload extras.
///
/// Covers the JSON-like values the engine round-trips through a posting
/// list. Date and datetime variants land with the SQL type system.
#[derive(Debug, Clone, Default)]
pub enum Value {
    #[default]
    Null,
    /// Non-null zero-width value of `PostgreSQL`'s `void` pseudo-type.
    Void,
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
    /// Validated `PostgreSQL` `json` text. Unlike `jsonb`, the original
    /// whitespace and object-key order are preserved.
    Json(String),
    /// Canonical `PostgreSQL` `jsonb` text. Keeping a distinct carrier
    /// preserves JSON scalars and arrays across SQL, storage, and bindings.
    JsonB(String),
    /// SQL array value with `PostgreSQL` dimension lower bounds.
    Array(ArrayValue),
    /// Internal generic sequence carrier for vectors, tensors, JSON arrays,
    /// graph lists, and callback payloads. SQL arrays use [`Value::Array`].
    List(Vec<Value>),
    /// Anonymous `ROW(...)` constructor. This stays distinct from arrays so
    /// row comparisons retain SQL three-valued NULL semantics.
    Row(Vec<Value>),
    /// Named composite/record value in physical field order.
    Record(Vec<(String, Value)>),
    /// JSON/document object value. This is not a SQL composite record.
    Map(BTreeMap<String, Value>),
}

#[derive(Serialize)]
struct TaggedText<'a> {
    #[serde(rename = "$uqa_type")]
    kind: &'static str,
    value: &'a str,
}

#[derive(Serialize)]
struct TaggedUnit {
    #[serde(rename = "$uqa_type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct TaggedBytes<'a> {
    #[serde(rename = "$uqa_type")]
    kind: &'static str,
    hex: &'a str,
}

#[derive(Serialize)]
struct TaggedArray<'a> {
    #[serde(rename = "$uqa_type")]
    kind: &'static str,
    lower_bounds: &'a [i32],
    values: &'a [Value],
}

#[derive(Serialize)]
struct TaggedRow<'a> {
    #[serde(rename = "$uqa_type")]
    kind: &'static str,
    values: &'a [Value],
}

#[derive(Serialize)]
struct TaggedRecord<'a> {
    #[serde(rename = "$uqa_type")]
    kind: &'static str,
    fields: &'a [(String, Value)],
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Void => TaggedUnit { kind: "void" }.serialize(serializer),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Float(value) => serializer.serialize_f64(*value),
            Self::Str(value) => serializer.serialize_str(value),
            Self::FixedChar(value) => TaggedText {
                kind: "fixed_char",
                value,
            }
            .serialize(serializer),
            Self::Bytes(value) => {
                const DIGITS: &[u8; 16] = b"0123456789abcdef";

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
            Self::Json(value) | Self::JsonB(value) => TaggedText {
                kind: if matches!(self, Self::Json(_)) {
                    "json"
                } else {
                    "jsonb"
                },
                value,
            }
            .serialize(serializer),
            Self::Array(value) => TaggedArray {
                kind: "array",
                lower_bounds: value.lower_bounds(),
                values: value.elements(),
            }
            .serialize(serializer),
            Self::List(value) => value.serialize(serializer),
            Self::Row(values) => TaggedRow {
                kind: "row",
                values,
            }
            .serialize(serializer),
            Self::Record(fields) => TaggedRecord {
                kind: "record",
                fields,
            }
            .serialize(serializer),
            Self::Map(value) => value.serialize(serializer),
        }
    }
}

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
    if matches!(
        tag,
        "date" | "time" | "time_tz" | "timestamp" | "timestamp_tz" | "interval"
    ) {
        return Ok(tagged_temporal_value(tag, map).map(Value::Temporal));
    }
    match tag {
        "void" if map.len() == 1 => Ok(Some(Value::Void)),
        "decimal" => {
            let Some(Value::Str(text)) = map.get("value") else {
                return Ok(None);
            };
            Ok(DecimalValue::parse(text).map(Value::Decimal))
        }
        "fixed_char" if map.len() == 2 => {
            let Some(Value::Str(text)) = map.get("value") else {
                return Ok(None);
            };
            Ok(Some(Value::FixedChar(text.clone())))
        }
        "bytes" if map.len() == 2 => {
            let Some(Value::Str(hex)) = map.get("hex") else {
                return Ok(None);
            };
            decode_hex_bytes(hex).map(|bytes| bytes.map(Value::Bytes))
        }
        "json" | "jsonb" if map.len() == 2 => {
            let Some(Value::Str(text)) = map.get("value") else {
                return Ok(None);
            };
            Ok(Some(if tag == "json" {
                Value::Json(text.clone())
            } else {
                Value::JsonB(text.clone())
            }))
        }
        "array" if map.len() == 3 => {
            let (Some(Value::List(lower_bounds)), Some(Value::List(values))) =
                (map.get("lower_bounds"), map.get("values"))
            else {
                return Ok(None);
            };
            let lower_bounds = lower_bounds
                .iter()
                .map(|value| match value {
                    Value::Int(value) => i32::try_from(*value).ok(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            Ok(lower_bounds.and_then(|lower_bounds| {
                ArrayValue::with_lower_bounds(values.clone(), lower_bounds).map(Value::Array)
            }))
        }
        "row" if map.len() == 2 => {
            let Some(Value::List(values)) = map.get("values") else {
                return Ok(None);
            };
            Ok(Some(Value::Row(values.clone())))
        }
        "record" if map.len() == 2 => {
            let Some(Value::List(encoded_fields)) = map.get("fields") else {
                return Ok(None);
            };
            let mut fields = Vec::new();
            fields
                .try_reserve_exact(encoded_fields.len())
                .map_err(|error| format!("cannot allocate decoded record fields: {error}"))?;
            for encoded in encoded_fields {
                let Value::List(pair) = encoded else {
                    return Ok(None);
                };
                let [Value::Str(name), value] = pair.as_slice() else {
                    return Ok(None);
                };
                fields.push((name.clone(), value.clone()));
            }
            Ok(Some(Value::Record(fields)))
        }
        _ => Ok(None),
    }
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
        if map.len() == 1 {
            if let Some(Value::Str(number)) = map.get("$serde_json::private::Number") {
                if let Ok(integer) = number.parse::<i64>() {
                    return Ok(Value::Int(integer));
                }
                if let Ok(float) = number.parse::<f64>() {
                    if float.is_finite() {
                        return Ok(Value::Float(float));
                    }
                }
                if let Some(decimal) = DecimalValue::parse(number) {
                    return Ok(Value::Decimal(decimal));
                }
                return Err(<A::Error as serde::de::Error>::custom(
                    "invalid arbitrary-precision JSON number",
                ));
            }
        }
        if let Some(Value::Str(tag)) = map.get("$uqa_type") {
            if let Some(value) =
                value_from_tagged_map(tag, &map).map_err(<A::Error as serde::de::Error>::custom)?
            {
                return Ok(value);
            }
        }
        Ok(Value::Map(map))
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
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
    if let Some(float_decimal) = DecimalValue::from_f64_lossy(float) {
        return float_decimal.cmp(decimal);
    }
    float.total_cmp(&0.0)
}

fn compare_postgres_container_values(left: &[Value], right: &[Value]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Greater,
            (_, Value::Null) => Ordering::Less,
            _ => left.cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

struct FlattenedArrayValues<'a> {
    stack: Vec<std::slice::Iter<'a, Value>>,
}

impl<'a> FlattenedArrayValues<'a> {
    fn new(values: &'a [Value]) -> Self {
        Self {
            stack: vec![values.iter()],
        }
    }
}

impl<'a> Iterator for FlattenedArrayValues<'a> {
    type Item = &'a Value;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let current = self.stack.last_mut()?;
            match current.next() {
                Some(Value::List(values)) => self.stack.push(values.iter()),
                Some(Value::Array(array)) => self.stack.push(array.elements().iter()),
                Some(value) => return Some(value),
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

fn compare_postgres_arrays(left: &ArrayValue, right: &ArrayValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut left_values = FlattenedArrayValues::new(left.elements());
    let mut right_values = FlattenedArrayValues::new(right.elements());
    loop {
        let ordering = match (left_values.next(), right_values.next()) {
            (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
            (Some(Value::Null), Some(_)) | (Some(_), None) => Ordering::Greater,
            (Some(_), Some(Value::Null)) | (None, Some(_)) => Ordering::Less,
            (Some(left), Some(right)) => left.cmp(right),
            (None, None) => break,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.dimensions()
        .len()
        .cmp(&right.dimensions().len())
        .then_with(|| left.dimensions().cmp(right.dimensions()))
        .then_with(|| left.lower_bounds().cmp(right.lower_bounds()))
}

// `Value` carries `f64`, so equality and ordering are implemented together.
// NaN compares equal to NaN and greater than finite values, signed zeroes are
// equal, and cross-numeric variants use numeric rather than discriminant order.
// PostgreSQL's B-tree ordering for array/composite fields considers NULL equal
// to NULL and greater than a non-NULL field. This keeps `Eq`/`Ord` consistent
// for BTree keys used by joins, DISTINCT, array_sort, and MIN/MAX.
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
            (Value::Null, Value::Null) | (Value::Void, Value::Void) => Ordering::Equal,
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
            (Value::Str(a), Value::Str(b)) | (Value::Json(a), Value::Json(b)) => a.cmp(b),
            (Value::JsonB(a), Value::JsonB(b)) => compare_jsonb_text(a, b),
            (Value::FixedChar(a), Value::FixedChar(b)) => {
                fixed_char_text(a).cmp(fixed_char_text(b))
            }
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            (Value::Temporal(a), Value::Temporal(b)) => a.cmp(b),
            (Value::Array(a), Value::Array(b)) => compare_postgres_arrays(a, b),
            (Value::List(a), Value::List(b)) | (Value::Row(a), Value::Row(b)) => {
                compare_postgres_container_values(a, b)
            }
            (Value::Record(a), Value::Record(b)) => compare_postgres_record_values(a, b),
            (Value::Map(a), Value::Map(b)) => a.cmp(b),
            _ => discriminant(self).cmp(&discriminant(other)),
        }
    }
}

fn discriminant(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Void => 1,
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Decimal(_) => 2,
        Value::Str(_) => 3,
        Value::FixedChar(_) => 4,
        Value::Bytes(_) => 5,
        Value::Temporal(_) => 6,
        Value::Json(_) => 7,
        Value::JsonB(_) => 8,
        Value::Array(_) => 9,
        Value::List(_) => 10,
        Value::Row(_) => 11,
        Value::Record(_) => 12,
        Value::Map(_) => 13,
    }
}

fn compare_postgres_record_values(
    left: &[(String, Value)],
    right: &[(String, Value)],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for ((_, left), (_, right)) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Greater,
            (_, Value::Null) => Ordering::Less,
            _ => left.cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn fixed_char_text(value: &str) -> &str {
    value.trim_end_matches(' ')
}
