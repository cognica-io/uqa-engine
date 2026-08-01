//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite/JSON/temporal value conversion and catalog JSON helpers.

use super::{
    blob_to_f32_vec, python_temporal_type, vector_value, BTreeMap, ColumnType, Connection,
    PythonColumnDef, PythonMigrationError, TemporalValue, Value, ValueRef,
};

pub(super) fn table_columns(
    conn: &Connection,
    table: &str,
) -> Result<Vec<String>, PythonMigrationError> {
    let sql = format!("PRAGMA table_info({})", quote_ident(table));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub(super) fn sqlite_value_to_uqa(
    raw: ValueRef<'_>,
    col: &PythonColumnDef,
) -> Result<Value, PythonMigrationError> {
    let lower = col.type_name.to_ascii_lowercase();
    match raw {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(n) if matches!(lower.as_str(), "bool" | "boolean") => {
            Ok(Value::Bool(n != 0))
        }
        ValueRef::Integer(n) if python_temporal_type(&lower).is_some() => {
            integer_to_temporal_value(n, &lower)
        }
        ValueRef::Integer(n) => Ok(Value::Int(n)),
        ValueRef::Real(n) => Ok(Value::Float(n)),
        ValueRef::Text(bytes) => {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| PythonMigrationError::Invalid(format!("invalid text: {e}")))?;
            if let Some(value) = text_to_temporal_value(text, &lower)? {
                return Ok(value);
            }
            if lower == "json"
                || lower == "jsonb"
                || lower == "vector"
                || lower == "point"
                || lower.ends_with("[]")
            {
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(json) => json_to_value(&json),
                    Err(_) => Ok(Value::Str(text.to_string())),
                }
            } else {
                Ok(Value::Str(text.to_string()))
            }
        }
        ValueRef::Blob(bytes) => {
            if lower == "vector" {
                Ok(vector_value(&blob_to_f32_vec(bytes)?))
            } else {
                Ok(Value::Bytes(bytes.to_vec()))
            }
        }
    }
}

pub(super) fn integer_to_temporal_value(
    value: i64,
    raw_type: &str,
) -> Result<Value, PythonMigrationError> {
    let ty = python_temporal_type(raw_type)
        .ok_or_else(|| PythonMigrationError::Invalid(format!("not a temporal type: {raw_type}")))?;
    let temporal = match ty {
        ColumnType::Date => TemporalValue::Date {
            days: i32::try_from(value).map_err(|e| {
                PythonMigrationError::Invalid(format!("date day offset {value} out of range: {e}"))
            })?,
        },
        ColumnType::Time => TemporalValue::Time { micros: value },
        ColumnType::TimeTz => TemporalValue::TimeTz {
            micros: value,
            offset_minutes: 0,
        },
        ColumnType::Timestamp => TemporalValue::Timestamp { micros: value },
        ColumnType::TimestampTz => TemporalValue::TimestampTz { micros: value },
        other => {
            return Err(PythonMigrationError::Invalid(format!(
                "temporal type resolver returned non-temporal type {other:?} for {raw_type}"
            )))
        }
    };
    Ok(Value::Temporal(temporal))
}

pub(super) fn text_to_temporal_value(
    text: &str,
    raw_type: &str,
) -> Result<Option<Value>, PythonMigrationError> {
    let Some(ty) = python_temporal_type(raw_type) else {
        return Ok(None);
    };
    let parsed = match ty {
        ColumnType::Date => TemporalValue::parse_date(text),
        ColumnType::Time => TemporalValue::parse_time(text),
        ColumnType::TimeTz => TemporalValue::parse_time_tz(text),
        ColumnType::Timestamp => TemporalValue::parse_timestamp(text),
        ColumnType::TimestampTz => TemporalValue::parse_timestamp_tz(text),
        _ => None,
    }
    .ok_or_else(|| {
        PythonMigrationError::Invalid(format!("invalid {raw_type} temporal value: {text}"))
    })?;
    Ok(Some(Value::Temporal(parsed)))
}

pub(super) fn json_to_value(json: &serde_json::Value) -> Result<Value, PythonMigrationError> {
    match json {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(v) => Ok(Value::Bool(*v)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Float(f))
            } else {
                Err(PythonMigrationError::Invalid(format!(
                    "unsupported JSON number {n}"
                )))
            }
        }
        serde_json::Value::String(s) => Ok(Value::Str(s.clone())),
        serde_json::Value::Array(items) => items
            .iter()
            .map(json_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        serde_json::Value::Object(items) => {
            if let Ok(temporal) = serde_json::from_value::<TemporalValue>(json.clone()) {
                return Ok(Value::Temporal(temporal));
            }
            items
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_value(value)?)))
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Value::Map)
        }
    }
}

pub(super) fn json_object_to_value_map(
    json: &str,
) -> Result<BTreeMap<String, Value>, PythonMigrationError> {
    match json_to_value(&serde_json::from_str::<serde_json::Value>(json)?)? {
        Value::Map(map) => Ok(map),
        other => Err(PythonMigrationError::Invalid(format!(
            "catalog properties must be a JSON object, got {other:?}"
        ))),
    }
}

pub(super) fn json_object_to_pairs(
    json: &str,
) -> Result<Vec<(String, String)>, PythonMigrationError> {
    Ok(parameters_to_string_map(json)?.into_iter().collect())
}

pub(super) fn parameters_to_string_map(
    json: &str,
) -> Result<BTreeMap<String, String>, PythonMigrationError> {
    let value = serde_json::from_str::<serde_json::Value>(json)?;
    let map = value.as_object().ok_or_else(|| {
        PythonMigrationError::Invalid("catalog options must be a JSON object".into())
    })?;
    Ok(map
        .iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            (key.clone(), value)
        })
        .collect())
}

pub(super) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
