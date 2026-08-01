//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JavaScript and core-value conversion.

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
            Value::Null => Null::to_napi_value(env, Null),
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
            Value::Str(value) => String::to_napi_value(env, value),
            Value::Bytes(value) => Buffer::to_napi_value(env, Buffer::from(value)),
            Value::Temporal(value) => String::to_napi_value(env, value.to_sql_string()),
            Value::List(values) => {
                Vec::<JSValue>::to_napi_value(env, values.into_iter().map(JSValue).collect())
            }
            Value::Map(values) => BTreeMap::<String, JSValue>::to_napi_value(
                env,
                values
                    .into_iter()
                    .map(|(key, value)| (key, JSValue(value)))
                    .collect(),
            ),
        }
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
