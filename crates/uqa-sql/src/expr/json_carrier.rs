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
        Value::LegacyVector(vector) => {
            serde_json::Value::Array(vector.elements().iter().map(core_value_to_json).collect())
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
    value_to_text_with_control(value, &uqa_core::memory::ProductionControl::uncontrolled())
        .expect("ordinary carrier text production")
        .into_uncontrolled()
        .expect("ordinary carrier text")
}

pub fn value_to_text_with_control(
    value: &Value,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> crate::error::Result<uqa_core::memory::Produced<String>> {
    control.check()?;
    Ok(match value {
        Value::Null | Value::Void => control.copy_text("")?,
        Value::Bool(value) => control.format(format_args!("{value}"))?,
        Value::Int(value) => control.format(format_args!("{value}"))?,
        Value::Float(value) => control.format(format_args!("{value}"))?,
        Value::Decimal(value) => value.to_sql_string_with_control(control)?,
        Value::Str(value) | Value::Json(value) | Value::JsonB(value) => control.copy_text(value)?,
        Value::FixedChar(value) => control.copy_text(value.trim_end_matches(' '))?,
        Value::Bytes(value) => super::json::utf8_lossy_with_control(value, control)?,
        Value::Temporal(value) => value.to_sql_string_with_control(control)?,
        Value::Array(array) => {
            super::conversion::array_value_to_string_with_control(array, control)?
        }
        Value::List(_) | Value::Map(_) => {
            super::json::format_core_value_as_json_with_control(value, control)?
        }
        Value::Row(_) | Value::Record(_) | Value::LegacyVector(_) => {
            super::conversion::value_to_string_with_control(value, control)?
        }
    })
}
