//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless typed-value and numeric blob encoding.

use super::{
    allocation_error, blob_marker, blob_marker_info, value_blob_marker, BTreeMap, DecimalValue,
    Document, EncodedDocument, SQLiteError, SQLiteResult, TemporalValue, Value,
    MIN_NUMERIC_BLOB_VALUES, VALUE_BLOB_F64_LIST, VALUE_BLOB_F64_TENSOR, VALUE_BLOB_TYPED_JSON,
};

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(super) enum StoredValue {
    Null,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    List(Vec<StoredValue>),
    Map(BTreeMap<String, StoredValue>),
}

impl StoredValue {
    pub(super) fn from_value(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(value),
            Value::Int(value) => Self::Int(value),
            Value::Float(value) => Self::FloatBits(value.to_bits()),
            Value::Str(value) => Self::Str(value),
            Value::Bytes(value) => Self::Bytes(value),
            Value::Temporal(value) => Self::Temporal(value),
            Value::Decimal(value) => Self::Decimal(value),
            Value::List(values) => Self::List(values.into_iter().map(Self::from_value).collect()),
            Value::Map(values) => Self::Map(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from_value(value)))
                    .collect(),
            ),
        }
    }

    pub(super) fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(value),
            Self::Int(value) => Value::Int(value),
            Self::FloatBits(value) => Value::Float(f64::from_bits(value)),
            Self::Str(value) => Value::Str(value),
            Self::Bytes(value) => Value::Bytes(value),
            Self::Temporal(value) => Value::Temporal(value),
            Self::Decimal(value) => Value::Decimal(value),
            Self::List(values) => Value::List(values.into_iter().map(Self::into_value).collect()),
            Self::Map(values) => Value::Map(
                values
                    .into_iter()
                    .map(|(key, value)| (key, value.into_value()))
                    .collect(),
            ),
        }
    }
}

pub(super) fn value_requires_typed_encoding(value: &Value) -> bool {
    if blob_marker_info(value).is_some() {
        return true;
    }
    match value {
        Value::List(items) => {
            items.is_empty()
                || items
                    .iter()
                    .all(|item| matches!(item, Value::Int(value) if u8::try_from(*value).is_ok()))
                || items.iter().any(value_requires_typed_encoding)
        }
        Value::Map(values) => values.values().any(value_requires_typed_encoding),
        Value::Bytes(_) => true,
        _ => false,
    }
}

/// Decode the unversioned document-body representation written before
/// `Value::Bytes` gained an explicit tag. New ambiguous lists and nested byte
/// values are stored as typed blobs, so a byte-range JSON array that still
/// appears inline can only be a legacy body and keeps its historical meaning.
pub(super) fn decode_legacy_document_body(body: &str) -> SQLiteResult<Document> {
    let serde_json::Value::Object(fields) = serde_json::from_str(body)? else {
        return Err(SQLiteError::StorageBackend(
            "persisted document body is not a JSON object".into(),
        ));
    };
    fields
        .into_iter()
        .map(|(field, value)| Ok((field, decode_legacy_json_value(value)?)))
        .collect()
}

pub(super) fn decode_legacy_json_value(value: serde_json::Value) -> SQLiteResult<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_u64() {
                Ok(i64::try_from(value).map_or(Value::Float(value as f64), Value::Int))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(SQLiteError::StorageBackend(
                    "persisted document number is outside the supported numeric range".into(),
                ))
            }
        }
        serde_json::Value::String(value) => Ok(Value::Str(value)),
        serde_json::Value::Array(values) => {
            let mut decoded = Vec::new();
            decoded
                .try_reserve_exact(values.len())
                .map_err(|error| allocation_error("legacy document array", error))?;
            for value in values {
                decoded.push(decode_legacy_json_value(value)?);
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(decoded.len())
                .map_err(|error| allocation_error("legacy document byte array", error))?;
            for value in &decoded {
                let Value::Int(value) = value else {
                    return Ok(Value::List(decoded));
                };
                let Ok(value) = u8::try_from(*value) else {
                    return Ok(Value::List(decoded));
                };
                bytes.push(value);
            }
            Ok(Value::Bytes(bytes))
        }
        serde_json::Value::Object(values) => {
            if values.contains_key("$uqa_type") {
                let tagged = serde_json::Value::Object(values.clone());
                let decoded = serde_json::from_value::<Value>(tagged)?;
                if !matches!(decoded, Value::Map(_)) {
                    return Ok(decoded);
                }
            }
            let mut decoded = BTreeMap::new();
            for (key, value) in values {
                decoded.insert(key, decode_legacy_json_value(value)?);
            }
            Ok(Value::Map(decoded))
        }
    }
}

pub(super) fn encode_typed_value(
    field: &str,
    value: Value,
) -> SQLiteResult<(Value, Option<Vec<u8>>)> {
    let bytes = serde_json::to_vec(&StoredValue::from_value(value))?;
    Ok((
        value_blob_marker(field.to_string(), VALUE_BLOB_TYPED_JSON),
        Some(bytes),
    ))
}

pub(super) fn encode_document_blobs(document: Document) -> SQLiteResult<EncodedDocument> {
    let mut stored = Document::new();
    let mut blobs = Vec::new();
    for (field, value) in document {
        let (stored_value, blob) = encode_stored_value(&field, value)?;
        stored.insert(field.clone(), stored_value);
        if let Some(bytes) = blob {
            blobs.push((field, bytes));
        }
    }
    Ok((stored, blobs))
}

pub(super) fn encode_stored_value(
    field: &str,
    value: Value,
) -> SQLiteResult<(Value, Option<Vec<u8>>)> {
    match value {
        Value::Bytes(bytes) => Ok((blob_marker(field.to_string()), Some(bytes))),
        Value::List(items) => {
            if let Some(bytes) = encode_f64_tensor_blob(&items)? {
                Ok((
                    value_blob_marker(field.to_string(), VALUE_BLOB_F64_TENSOR),
                    Some(bytes),
                ))
            } else if let Some(bytes) = encode_f64_list_blob(&items)? {
                Ok((
                    value_blob_marker(field.to_string(), VALUE_BLOB_F64_LIST),
                    Some(bytes),
                ))
            } else {
                let value = Value::List(items);
                if value_requires_typed_encoding(&value) {
                    encode_typed_value(field, value)
                } else {
                    Ok((value, None))
                }
            }
        }
        other if value_requires_typed_encoding(&other) => encode_typed_value(field, other),
        other => Ok((other, None)),
    }
}

pub(super) fn encode_f64_list_blob(items: &[Value]) -> SQLiteResult<Option<Vec<u8>>> {
    if items.len() < MIN_NUMERIC_BLOB_VALUES {
        return Ok(None);
    }
    let capacity = items
        .len()
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or_else(|| SQLiteError::StorageBackend("f64-list payload size overflow".into()))?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|error| allocation_error("f64-list payload", error))?;
    for item in items {
        let Some(value) = value_as_finite_f64(item) else {
            return Ok(None);
        };
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(Some(out))
}

pub(super) fn encode_f64_tensor_blob(items: &[Value]) -> SQLiteResult<Option<Vec<u8>>> {
    let rows = items.len();
    let Some(Value::List(first)) = items.first() else {
        return Ok(None);
    };
    let cols = first.len();
    let Some(value_count) = rows.checked_mul(cols) else {
        return Ok(None);
    };
    if rows == 0 || cols == 0 || value_count < MIN_NUMERIC_BLOB_VALUES {
        return Ok(None);
    }
    let Ok(rows_u32) = u32::try_from(rows) else {
        return Ok(None);
    };
    let Ok(cols_u32) = u32::try_from(cols) else {
        return Ok(None);
    };
    let payload_len = value_count
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or_else(|| SQLiteError::StorageBackend("f64-tensor payload size overflow".into()))?;
    let capacity = 8usize
        .checked_add(payload_len)
        .ok_or_else(|| SQLiteError::StorageBackend("f64-tensor payload size overflow".into()))?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|error| allocation_error("f64-tensor payload", error))?;
    out.extend_from_slice(&rows_u32.to_le_bytes());
    out.extend_from_slice(&cols_u32.to_le_bytes());
    for row in items {
        let Value::List(values) = row else {
            return Ok(None);
        };
        if values.len() != cols {
            return Ok(None);
        }
        for value in values {
            let Some(value) = value_as_finite_f64(value) else {
                return Ok(None);
            };
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(Some(out))
}

pub(super) fn value_as_finite_f64(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Float(value) => *value,
        _ => return None,
    };
    value.is_finite().then_some(value)
}
