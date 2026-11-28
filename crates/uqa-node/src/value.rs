//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JavaScript and core-value conversion.

use napi::bindgen_prelude::{Array, Env};

use super::{
    sys, BTreeMap, BigInt, Buffer, Error, Float32Array, Float64Array, FromNapiValue, JsValue, Null,
    Object, Result, ToNapiValue, TypeName, Uint8Array, Unknown, ValidateNapiValue, Value,
    ValueType, MAX_SAFE_INTEGER,
};

// ---------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------

/// Bidirectional bridge between engine [`Value`]s and JavaScript
/// values. Ints beyond `Number.MAX_SAFE_INTEGER` surface as `BigInt`,
/// bytes as `Buffer`, decimals and temporals as their SQL strings.
pub struct JSValue(pub(super) Value);

impl TypeName for JSValue {
    fn type_name() -> &'static str {
        "unknown"
    }

    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}

impl ValidateNapiValue for JSValue {}

impl ToNapiValue for JSValue {
    unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> Result<sys::napi_value> {
        unsafe { value_to_napi(env, val.0) }
    }
}

impl FromNapiValue for JSValue {
    unsafe fn from_napi_value(env: sys::napi_env, napi_val: sys::napi_value) -> Result<Self> {
        let unknown = unsafe { Unknown::from_raw_unchecked(env, napi_val) };
        Ok(Self(value_from_unknown(&unknown)?))
    }
}

pub(super) unsafe fn value_to_napi(env: sys::napi_env, value: Value) -> Result<sys::napi_value> {
    unsafe {
        match value {
            Value::Datum(datum) => value_to_napi(
                env,
                uqa_sql::expr::datums::read(&datum)
                    .map_err(|error| Error::from_reason(error.to_string()))?,
            ),
            Value::Null => Null::to_napi_value(env, Null),
            Value::Void => String::to_napi_value(env, String::new()),
            Value::Bool(value) => bool::to_napi_value(env, value),
            Value::Int(value) => {
                if value.unsigned_abs() <= MAX_SAFE_INTEGER as u64 {
                    i64::to_napi_value(env, value)
                } else {
                    BigInt::to_napi_value(env, BigInt::from(value))
                }
            }
            Value::Float(value) => f64::to_napi_value(env, value),
            Value::Decimal(value) => String::to_napi_value(env, value.to_sql_string()),
            Value::Str(value) | Value::FixedChar(value) => String::to_napi_value(env, value),
            Value::Json(value) | Value::JsonB(value) => {
                let parsed = serde_json::from_str(&value).map_err(|error| {
                    Error::from_reason(format!("invalid SQL JSON value: {error}"))
                })?;
                JSONValue::to_napi_value(env, JSONValue(parsed))
            }
            Value::Bytes(value) => Buffer::to_napi_value(env, Buffer::from(value)),
            value @ Value::BitString(_) => String::to_napi_value(
                env,
                uqa_sql::expr::value_to_string(&value)
                    .map_err(|error| Error::from_reason(error.to_string()))?,
            ),
            Value::Temporal(value) => String::to_napi_value(env, value.to_sql_string()),
            // Result conversion renders enum columns through the engine catalog before reaching this context-free converter.
            Value::Enum(value) => Err(Error::from_reason(
                uqa_sql::expr::catalog_output_required(&value).to_string(),
            )),
            Value::LegacyVector(vector) => value_to_napi(env, Value::Array(vector.into_array())),
            Value::Array(array) => {
                array_to_napi(env, array.into_elements().into_iter().map(JSValue))
            }
            Value::List(values) => array_to_napi(env, values.into_iter().map(JSValue)),
            Value::Row(row) => array_to_napi(env, row.into_values().into_iter().map(JSValue)),
            Value::Record(record) => {
                let (mut fields, _) = record.into_parts();
                // The object has sorted keys and the last value of each repeated field.
                fields.sort_by(|left, right| left.0.cmp(&right.0));
                fields.dedup_by(|next, first| {
                    if first.0 == next.0 {
                        std::mem::swap(&mut first.1, &mut next.1);
                        true
                    } else {
                        false
                    }
                });
                object_to_napi(
                    env,
                    fields.into_iter().map(|(key, value)| (key, JSValue(value))),
                )
            }
            Value::Map(row) => JSRow::to_napi_value(env, JSRow(row)),
        }
    }
}

unsafe fn array_to_napi<T: ToNapiValue>(
    raw: sys::napi_env,
    values: impl ExactSizeIterator<Item = T>,
) -> Result<sys::napi_value> {
    let env = Env::from_raw(raw);
    let len = u32::try_from(values.len())
        .map_err(|_| Error::from_reason("array length exceeds uint32"))?;
    let mut array = env.create_array(len)?;
    for (index, value) in (0..len).zip(values) {
        array.set(index, value)?;
    }
    unsafe { Array::to_napi_value(raw, array) }
}

unsafe fn object_to_napi<T: ToNapiValue>(
    raw: sys::napi_env,
    fields: impl IntoIterator<Item = (String, T)>,
) -> Result<sys::napi_value> {
    let env = Env::from_raw(raw);
    let mut object = Object::new(&env)?;
    for (key, value) in fields {
        object.set(key, value)?;
    }
    unsafe { Object::to_napi_value(raw, object) }
}

struct JSONValue(serde_json::Value);

impl ToNapiValue for JSONValue {
    unsafe fn to_napi_value(env: sys::napi_env, value: Self) -> Result<sys::napi_value> {
        use serde_json::Value as JSON;
        unsafe {
            match value.0 {
                JSON::Null => Null::to_napi_value(env, Null),
                JSON::Bool(value) => bool::to_napi_value(env, value),
                JSON::Number(value) => match value.as_i64() {
                    Some(value) => value_to_napi(env, Value::Int(value)),
                    None => f64::to_napi_value(env, value.as_f64().unwrap_or(f64::NAN)),
                },
                JSON::String(value) => String::to_napi_value(env, value),
                JSON::Array(values) => array_to_napi(env, values.into_iter().map(Self)),
                JSON::Object(mut values) => {
                    values.sort_keys();
                    object_to_napi(
                        env,
                        values.into_iter().map(|(key, value)| (key, Self(value))),
                    )
                }
            }
        }
    }
}

pub struct JSRow(pub(super) BTreeMap<String, Value>);
pub struct JSRows(pub(super) Vec<BTreeMap<String, Value>>);

impl ToNapiValue for JSRow {
    unsafe fn to_napi_value(env: sys::napi_env, row: Self) -> Result<sys::napi_value> {
        unsafe {
            object_to_napi(
                env,
                row.0.into_iter().map(|(key, value)| (key, JSValue(value))),
            )
        }
    }
}

impl FromNapiValue for JSRow {
    unsafe fn from_napi_value(env: sys::napi_env, value: sys::napi_value) -> Result<Self> {
        let row = unsafe { BTreeMap::<String, JSValue>::from_napi_value(env, value) }?;
        Ok(Self(document_from_js(row)))
    }
}

impl ToNapiValue for JSRows {
    unsafe fn to_napi_value(env: sys::napi_env, rows: Self) -> Result<sys::napi_value> {
        unsafe { array_to_napi(env, rows.0.into_iter().map(JSRow)) }
    }
}

impl FromNapiValue for JSRows {
    unsafe fn from_napi_value(env: sys::napi_env, value: sys::napi_value) -> Result<Self> {
        let rows = unsafe { Vec::<JSRow>::from_napi_value(env, value) }?;
        Ok(Self(rows.into_iter().map(|row| row.0).collect()))
    }
}

pub(super) fn value_from_unknown(value: &Unknown<'_>) -> Result<Value> {
    match value.get_type()? {
        ValueType::Undefined | ValueType::Null => Ok(Value::Null),
        ValueType::Boolean => Ok(Value::Bool(unsafe { value.cast::<bool>() }?)),
        ValueType::Number => {
            let number = unsafe { value.cast::<f64>() }?;
            value_from_js_number(number)
        }
        ValueType::BigInt => {
            let bigint = unsafe { value.cast::<BigInt>() }?;
            let (number, lossless) = bigint.get_i64();
            if lossless {
                Ok(Value::Int(number))
            } else {
                Err(Error::from_reason("BigInt value is outside i64 range"))
            }
        }
        ValueType::String => Ok(Value::Str(unsafe { value.cast::<String>() }?)),
        ValueType::Object => {
            if value.is_buffer()? {
                let buffer = unsafe { value.cast::<Buffer>() }?;
                return Ok(Value::Bytes(buffer.to_vec()));
            }
            if value.is_typedarray()? {
                if let Ok(bytes) = unsafe { value.cast::<Uint8Array>() } {
                    return Ok(Value::Bytes(bytes.to_vec()));
                }
                if let Ok(floats) = unsafe { value.cast::<Float64Array>() } {
                    return Ok(Value::List(
                        floats.iter().map(|value| Value::Float(*value)).collect(),
                    ));
                }
                if let Ok(floats) = unsafe { value.cast::<Float32Array>() } {
                    return Ok(Value::List(
                        floats
                            .iter()
                            .map(|value| Value::Float(f64::from(*value)))
                            .collect(),
                    ));
                }
                return Err(Error::from_reason(
                    "unsupported typed array; use Uint8Array, Float32Array, or Float64Array",
                ));
            }
            if value.is_array()? {
                let values = unsafe { value.cast::<Vec<JSValue>>() }?;
                return Ok(Value::List(
                    values.into_iter().map(|value| value.0).collect(),
                ));
            }
            if value.is_date()? {
                return Err(Error::from_reason(
                    "Date values are not supported; pass an ISO 8601 string instead",
                ));
            }
            let object = unsafe { value.cast::<Object>() }?;
            let mut map = BTreeMap::new();
            for key in Object::keys(&object)? {
                let entry: Option<JSValue> = object.get(&key)?;
                map.insert(key, entry.map_or(Value::Null, |value| value.0));
            }
            Ok(Value::Map(map))
        }
        other => Err(Error::from_reason(format!(
            "unsupported JavaScript value type: {other}"
        ))),
    }
}

pub(super) fn value_from_js_number(number: f64) -> Result<Value> {
    if number.is_finite() && number.fract() == 0.0 {
        if number.abs() > MAX_SAFE_INTEGER as f64 {
            return Err(Error::from_reason(format!(
                "integer-valued JavaScript Number {number} is outside the safe integer range; pass a BigInt"
            )));
        }
        return Ok(Value::Int(number as i64));
    }
    Ok(Value::Float(number))
}

pub(super) fn document_from_js(document: BTreeMap<String, JSValue>) -> BTreeMap<String, Value> {
    document
        .into_iter()
        .map(|(key, value)| (key, value.0))
        .collect()
}
