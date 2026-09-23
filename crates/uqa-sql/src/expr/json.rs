//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON scalar-function helpers for the expression evaluator.

use uqa_core::{DecimalValue, TemporalValue, Value};

use crate::error::{Result, SQLError};

use super::{hex_encode, out_of_range, value_to_string};

mod path;
mod production;
#[cfg(test)]
pub(super) use production::json_delete_with_control;
pub(super) use production::{
    cast_json_value_with_control, evaluate, format_core_value_as_json_with_control,
    format_value_as_json_with_control, json_concat_with_control, json_delete_values_with_control,
    json_extract_operator_with_control, quote_with_control, utf8_lossy_with_control,
};

pub(super) use path::{jsonpath_candidate, jsonpath_match};

pub(super) fn parse_json(s: &str) -> Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(s)
        .map_err(|_| super::json_strip::invalid_json_input(s))
}

/// Render a parsed JSON value in `PostgreSQL`'s compact result format. JSONB
/// objects use `PostgreSQL`'s length-then-bytewise key ordering.
pub(super) fn format_json(value: &serde_json::Value, jsonb: bool) -> String {
    if !jsonb {
        return serde_json::to_string(value).expect("serializing a JSON value cannot fail");
    }
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => {
            let text = value.to_string();
            DecimalValue::parse(&text).map_or(text, |value| value.to_sql_string())
        }
        serde_json::Value::String(value) => serde_json::Value::String(value.clone()).to_string(),
        serde_json::Value::Array(values) => {
            let values = values
                .iter()
                .map(|value| format_json(value, true))
                .collect::<Vec<_>>();
            format!("[{}]", values.join(", "))
        }
        serde_json::Value::Object(values) => {
            let mut values = values.iter().collect::<Vec<_>>();
            values.sort_by(|(left, _), (right, _)| {
                left.len()
                    .cmp(&right.len())
                    .then_with(|| left.as_bytes().cmp(right.as_bytes()))
            });
            let values = values
                .into_iter()
                .map(|(key, value)| {
                    let key = serde_json::Value::String(key.clone()).to_string();
                    format!("{key}: {}", format_json(value, true))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", values.join(", "))
        }
    }
}

#[cfg(test)]
fn format_jsonb_pretty(value: &serde_json::Value) -> String {
    let Value::Str(text) =
        ordinary("jsonb_pretty", &[Value::Json(value.to_string())]).expect("valid parsed JSON")
    else {
        unreachable!("JSON pretty text")
    };
    text
}

pub(super) fn typed_json_value(value: &serde_json::Value, jsonb: bool) -> Result<Value> {
    if jsonb {
        validate_jsonb_numbers(value)?;
    }
    let text = format_json(value, jsonb);
    if jsonb {
        Ok(Value::JsonB(text))
    } else {
        Ok(Value::Json(text))
    }
}

fn validate_jsonb_numbers(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Number(value) => DecimalValue::parse(&value.to_string())
            .map(|_| ())
            .ok_or_else(|| out_of_range("numeric")),
        serde_json::Value::Array(values) => values.iter().try_for_each(validate_jsonb_numbers),
        serde_json::Value::Object(values) => values.values().try_for_each(validate_jsonb_numbers),
        _ => Ok(()),
    }
}

/// Render an engine value as `PostgreSQL` JSON input text without losing the
/// lexical representation of values already typed as `json` or `jsonb`.
pub fn value_to_json_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Void => "\"\"".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => match serde_json::Number::from_f64(*value) {
            Some(number) => number.to_string(),
            None if value.is_nan() => "\"NaN\"".to_string(),
            None if value.is_sign_positive() => "\"Infinity\"".to_string(),
            None => "\"-Infinity\"".to_string(),
        },
        Value::Decimal(value) => value.to_sql_string(),
        Value::Str(value) => serde_json::Value::String(value.clone()).to_string(),
        Value::FixedChar(value) => {
            serde_json::Value::String(value.trim_end_matches(' ').to_string()).to_string()
        }
        Value::Bytes(value) => {
            serde_json::Value::String(format!("0x{}", hex_encode(value))).to_string()
        }
        Value::Temporal(value) => serde_json::Value::String(value.to_sql_string()).to_string(),
        Value::Json(text) | Value::JsonB(text) => text.clone(),
        Value::Array(array) => {
            let values = array
                .elements()
                .iter()
                .map(value_to_json_text)
                .collect::<Vec<_>>();
            format!("[{}]", values.join(","))
        }
        Value::List(values) => {
            let values = values.iter().map(value_to_json_text).collect::<Vec<_>>();
            format!("[{}]", values.join(","))
        }
        Value::Row(values) => record_json_text(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (format!("f{}", index + 1), value)),
        ),
        Value::Record(fields) => {
            record_json_text(fields.iter().map(|(name, value)| (name.clone(), value)))
        }
        Value::Map(values) => {
            let values = values
                .iter()
                .map(|(key, value)| {
                    let key = serde_json::Value::String(key.clone()).to_string();
                    format!("{key}:{}", value_to_json_text(value))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", values.join(","))
        }
    }
}

fn record_json_text<'a>(fields: impl IntoIterator<Item = (String, &'a Value)>) -> String {
    let fields = fields
        .into_iter()
        .map(|(name, value)| {
            let name = serde_json::Value::String(name).to_string();
            format!("{name}:{}", value_to_json_text(value))
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", fields.join(","))
}

pub(super) fn json_build_array_value(args: &[Value], jsonb: bool) -> Result<Value> {
    let text = format!(
        "[{}]",
        args.iter()
            .map(value_to_json_text)
            .collect::<Vec<_>>()
            .join(", ")
    );
    if jsonb {
        typed_json_value(&parse_json(&text)?, true)
    } else {
        Ok(Value::Json(text))
    }
}

pub(super) fn json_build_object_value(args: &[Value], jsonb: bool) -> Result<Value> {
    if !args.len().is_multiple_of(2) {
        return Err(SQLError::TypeMismatch(
            "json_build_object requires an even number of args".into(),
        ));
    }
    let mut fields = Vec::with_capacity(args.len() / 2);
    for pair in args.chunks_exact(2) {
        if matches!(pair[0], Value::Null) {
            return Err(SQLError::TypeMismatch(
                "json_build_object key must not be NULL".into(),
            ));
        }
        let key = serde_json::Value::String(value_to_string(&pair[0])).to_string();
        fields.push(format!("{key} : {}", value_to_json_text(&pair[1])));
    }
    let text = format!("{{{}}}", fields.join(", "));
    if jsonb {
        typed_json_value(&parse_json(&text)?, true)
    } else {
        Ok(Value::Json(text))
    }
}

#[cfg(test)]
pub(super) fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
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
        Value::Decimal(d) => {
            if d.is_nan() || d.is_infinite() {
                serde_json::Value::String(d.to_sql_string())
            } else {
                d.to_sql_string()
                    .parse::<serde_json::Number>()
                    .map(serde_json::Value::Number)
                    .unwrap_or_else(|_| serde_json::Value::String(d.to_sql_string()))
            }
        }
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::FixedChar(s) => serde_json::Value::String(s.trim_end_matches(' ').to_string()),
        Value::Bytes(b) => serde_json::Value::String(format!("0x{}", hex_encode(b))),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::Json(text) | Value::JsonB(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.clone()))
        }
        Value::Array(array) => {
            serde_json::Value::Array(array.elements().iter().map(value_to_json).collect())
        }
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Row(values) => serde_json::Value::Object(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (format!("f{}", index + 1), value_to_json(value)))
                .collect(),
        ),
        Value::Record(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), value_to_json(value)))
                .collect(),
        ),
        Value::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                obj.insert(k.clone(), value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
    }
}

#[allow(dead_code)]
pub(super) fn json_to_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(d) = DecimalValue::parse(&n.to_string()) {
                Value::Decimal(d)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => Value::List(arr.iter().map(json_to_value).collect()),
        serde_json::Value::Object(obj) => {
            if let Ok(temporal) =
                serde_json::from_value::<TemporalValue>(serde_json::Value::Object(obj.clone()))
            {
                return Value::Temporal(temporal);
            }
            let mut map = std::collections::BTreeMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_value(v));
            }
            Value::Map(map)
        }
    }
}

fn ordinary(name: &str, args: &[Value]) -> Result<Value> {
    Ok(production::evaluate(
        name,
        args,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("known JSON builtin")?
    .into_uncontrolled()
    .expect("ordinary JSON has no lease"))
}

pub(super) fn json_extract_operator(args: &[Value], as_text: bool, path: bool) -> Result<Value> {
    Ok(json_extract_operator_with_control(
        args,
        as_text,
        path,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary JSON extraction has no lease"))
}

fn json_array_index(len: usize, key: &str) -> Option<usize> {
    let index = key.parse::<i64>().ok()?;
    let normalized = if index < 0 { len as i64 + index } else { index };
    usize::try_from(normalized).ok().filter(|idx| *idx < len)
}

fn json_insert_index(len: usize, key: &str, insert_after: bool) -> Option<usize> {
    let raw = key.parse::<i64>().ok()?;
    let len_i64 = len as i64;
    let index = if raw >= 0 {
        if raw >= len_i64 {
            len_i64
        } else if insert_after {
            raw + 1
        } else {
            raw
        }
    } else {
        let normalized = len_i64 + raw;
        if normalized < 0 {
            0
        } else if insert_after {
            normalized + 1
        } else {
            normalized
        }
    };
    usize::try_from(index.clamp(0, len_i64)).ok()
}

#[cfg(test)]
mod pretty_tests {
    use super::{format_jsonb_pretty, parse_json, typed_json_value, DecimalValue};

    #[test]
    fn jsonb_pretty_uses_postgresql_layout_and_key_order() {
        let value = parse_json(r#"{"zz":1,"b":[],"aa":{"long":3,"x":2}}"#).unwrap();
        assert_eq!(
            format_jsonb_pretty(&value),
            "{\n    \"b\": [\n    ],\n    \"aa\": {\n        \"x\": 2,\n        \"long\": 3\n    },\n    \"zz\": 1\n}"
        );
        assert_eq!(format_jsonb_pretty(&parse_json("[]").unwrap()), "[\n]");
        assert_eq!(format_jsonb_pretty(&parse_json("{}").unwrap()), "{\n}");
        assert_eq!(
            format_jsonb_pretty(&parse_json("1e-1000").unwrap()),
            DecimalValue::parse("1e-1000").unwrap().to_sql_string()
        );
        assert_eq!(format_jsonb_pretty(&parse_json("1.00").unwrap()), "1.00");
        assert_eq!(format_jsonb_pretty(&parse_json("-0").unwrap()), "0");
    }

    #[test]
    fn jsonb_rejects_numbers_outside_postgresql_numeric_range() {
        let maximum = parse_json("1e131071").unwrap();
        assert!(typed_json_value(&maximum, true).is_ok());

        for text in ["1e131072", "1e-16384", "[1e131072]", r#"{"n":1e131072}"#] {
            let error = typed_json_value(&parse_json(text).unwrap(), true).unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
        }

        assert!(typed_json_value(&parse_json("1e200000").unwrap(), false).is_ok());
        assert!(typed_json_value(&parse_json("0e200000").unwrap(), true).is_ok());
    }
}
