//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert SQL values into the JSON carrier used by table and aggregate execution.

use uqa_core::Value;

pub fn core_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Void => serde_json::Value::String(String::new()),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f).map_or_else(
            || {
                let label = if f.is_nan() {
                    "NaN"
                } else if f.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                serde_json::Value::String(label.to_string())
            },
            serde_json::Value::Number,
        ),
        Value::Decimal(d) => d
            .to_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(
                || serde_json::Value::String(d.to_sql_string()),
                serde_json::Value::Number,
            ),
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
        Value::FixedChar(s) => serde_json::Value::String(s.trim_end_matches(' ').to_string()),
        Value::Bytes(bytes) => serde_json::Value::String(String::from_utf8_lossy(bytes).into()),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::Json(text) | Value::JsonB(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.clone()))
        }
        Value::Array(array) => {
            serde_json::Value::Array(array.elements().iter().map(core_value_to_json).collect())
        }
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(core_value_to_json).collect())
        }
        Value::Row(values) => serde_json::Value::Object(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (format!("f{}", index + 1), core_value_to_json(value)))
                .collect(),
        ),
        Value::Record(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), core_value_to_json(value)))
                .collect(),
        ),
        Value::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), core_value_to_json(v)))
                .collect(),
        ),
    }
}

pub fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Void => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::FixedChar(s) => s.trim_end_matches(' ').to_string(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::Json(text) | Value::JsonB(text) => text.clone(),
        Value::Array(array) => crate::expr::array_value_to_string(array),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
        Value::Row(_) | Value::Record(_) => crate::expr::value_to_string(value),
    }
}
