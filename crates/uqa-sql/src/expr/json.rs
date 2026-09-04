//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON scalar-function helpers for the expression evaluator.

use uqa_core::{jsonb_equality_key, DecimalValue, TemporalValue, Value};

use crate::error::{Result, SQLError};

use super::{hex_encode, out_of_range, value_to_string};

mod path;

pub(super) use path::{jsonpath_candidate, jsonpath_exists, jsonpath_match};

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

pub(super) fn format_jsonb_pretty(value: &serde_json::Value) -> String {
    format_jsonb_pretty_at_depth(value, 0)
}

fn format_jsonb_pretty_at_depth(value: &serde_json::Value, depth: usize) -> String {
    let indent = " ".repeat(depth * 4);
    let child_indent = " ".repeat((depth + 1) * 4);
    match value {
        serde_json::Value::Array(values) => {
            if values.is_empty() {
                return format!("[\n{indent}]");
            }
            let values = values
                .iter()
                .map(|value| {
                    format!(
                        "{child_indent}{}",
                        format_jsonb_pretty_at_depth(value, depth + 1)
                    )
                })
                .collect::<Vec<_>>();
            format!("[\n{}\n{indent}]", values.join(",\n"))
        }
        serde_json::Value::Object(values) => {
            if values.is_empty() {
                return format!("{{\n{indent}}}");
            }
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
                    format!(
                        "{child_indent}{key}: {}",
                        format_jsonb_pretty_at_depth(value, depth + 1)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{\n{}\n{indent}}}", values.join(",\n"))
        }
        _ => format_json(value, true),
    }
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

pub(super) fn json_typeof(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub(super) fn json_extract_path(args: &[Value], as_text: bool, jsonb: bool) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "json_extract_path takes 2+ args".into(),
        ));
    }
    let jsonb = jsonb || matches!(args[0], Value::JsonB(_));
    let mut current = parse_json(&value_to_string(&args[0]))?;
    for key in &args[1..] {
        let key_str = value_to_string(key);
        current = match current {
            serde_json::Value::Object(mut obj) => {
                obj.remove(&key_str).unwrap_or(serde_json::Value::Null)
            }
            serde_json::Value::Array(arr) => json_array_index(arr.len(), &key_str)
                .and_then(|idx| arr.into_iter().nth(idx))
                .unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::Null,
        };
    }
    if as_text {
        Ok(Value::Str(match current {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => return Ok(Value::Null),
            other => format_json(&other, jsonb),
        }))
    } else if matches!(current, serde_json::Value::Null) {
        Ok(Value::Null)
    } else {
        typed_json_value(&current, jsonb)
    }
}

fn json_array_index(len: usize, key: &str) -> Option<usize> {
    let index = key.parse::<i64>().ok()?;
    let normalized = if index < 0 { len as i64 + index } else { index };
    usize::try_from(normalized).ok().filter(|idx| *idx < len)
}

pub(super) fn json_contains(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_contains takes 2 args".into()));
    }
    let lhs = parse_json(&value_to_string(&args[0]))?;
    let rhs = parse_json(&value_to_string(&args[1]))?;
    Ok(Value::Bool(json_contains_value(&lhs, &rhs)))
}

pub(super) fn json_contained_by(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "json_contained_by takes 2 args".into(),
        ));
    }
    let lhs = parse_json(&value_to_string(&args[0]))?;
    let rhs = parse_json(&value_to_string(&args[1]))?;
    Ok(Value::Bool(json_contains_value(&rhs, &lhs)))
}

fn json_contains_value(lhs: &serde_json::Value, rhs: &serde_json::Value) -> bool {
    json_contains_value_at_depth(lhs, rhs, true)
}

fn json_contains_value_at_depth(
    lhs: &serde_json::Value,
    rhs: &serde_json::Value,
    top_level: bool,
) -> bool {
    match (lhs, rhs) {
        (serde_json::Value::Object(l), serde_json::Value::Object(r)) => r.iter().all(|(k, rv)| {
            l.get(k)
                .is_some_and(|lv| json_contains_value_at_depth(lv, rv, false))
        }),
        (serde_json::Value::Array(l), serde_json::Value::Array(r)) => r.iter().all(|rv| {
            l.iter()
                .any(|lv| json_contains_value_at_depth(lv, rv, false))
        }),
        (serde_json::Value::Array(l), r) if top_level && jsonb_is_primitive(r) => l
            .iter()
            .any(|lv| json_contains_value_at_depth(lv, r, false)),
        _ => jsonb_values_equal(lhs, rhs),
    }
}

fn jsonb_is_primitive(value: &serde_json::Value) -> bool {
    !matches!(
        value,
        serde_json::Value::Array(_) | serde_json::Value::Object(_)
    )
}

fn jsonb_values_equal(lhs: &serde_json::Value, rhs: &serde_json::Value) -> bool {
    let lhs = serde_json::to_string(lhs).expect("serializing parsed JSON cannot fail");
    let rhs = serde_json::to_string(rhs).expect("serializing parsed JSON cannot fail");
    match (jsonb_equality_key(&lhs), jsonb_equality_key(&rhs)) {
        (Some(lhs), Some(rhs)) => lhs == rhs,
        _ => false,
    }
}

pub(super) fn json_has_key(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_has_key takes 2 args".into()));
    }
    let obj = parse_json(&value_to_string(&args[0]))?;
    let key = value_to_string(&args[1]);
    Ok(Value::Bool(match obj {
        serde_json::Value::Object(map) => map.contains_key(&key),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| matches!(item, serde_json::Value::String(value) if value == &key)),
        _ => false,
    }))
}

pub(super) fn json_has_keys(args: &[Value], require_all: bool) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_has_keys takes 2 args".into()));
    }
    let obj = parse_json(&value_to_string(&args[0]))?;
    let keys = match &args[1] {
        Value::Array(array) => array_strings(array.elements()),
        Value::List(items) => array_strings(items),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "json key list must be array, got {other:?}"
            )));
        }
    };
    let found = |key: &String| match &obj {
        serde_json::Value::Object(map) => map.contains_key(key),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| matches!(item, serde_json::Value::String(value) if value == key)),
        _ => false,
    };
    Ok(Value::Bool(if require_all {
        keys.iter().all(found)
    } else {
        keys.iter().any(found)
    }))
}

pub(super) fn json_concat(args: &[Value]) -> Result<Option<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_concat takes 2 args".into()));
    }
    if !args.iter().any(|arg| matches!(arg, Value::JsonB(_))) {
        return Ok(None);
    }
    let lhs = value_to_json(&args[0]);
    let rhs = value_to_json(&args[1]);
    let out = match (lhs, rhs) {
        (serde_json::Value::Object(mut left), serde_json::Value::Object(right)) => {
            for (key, value) in right {
                left.insert(key, value);
            }
            serde_json::Value::Object(left)
        }
        (serde_json::Value::Array(mut left), serde_json::Value::Array(right)) => {
            left.extend(right);
            serde_json::Value::Array(left)
        }
        (serde_json::Value::Array(mut left), right) => {
            left.push(right);
            serde_json::Value::Array(left)
        }
        (left, serde_json::Value::Array(mut right)) => {
            let mut out = vec![left];
            out.append(&mut right);
            serde_json::Value::Array(out)
        }
        (left, right) => serde_json::Value::Array(vec![left, right]),
    };
    typed_json_value(&out, true).map(Some)
}

pub(super) fn json_delete(args: &[Value]) -> Result<Option<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_delete takes 2 args".into()));
    }
    if !matches!(args[0], Value::JsonB(_) | Value::Map(_) | Value::List(_)) {
        return Ok(None);
    }
    let mut target = value_to_json(&args[0]);
    match &args[1] {
        Value::Int(index) => delete_array_index(&mut target, *index),
        Value::Array(array) => {
            for key in array_strings(array.elements()) {
                delete_key_or_string(&mut target, &key);
            }
        }
        Value::List(keys) => {
            for key in array_strings(keys) {
                delete_key_or_string(&mut target, &key);
            }
        }
        key => delete_key_or_string(&mut target, &value_to_string(key)),
    }
    typed_json_value(&target, true).map(Some)
}

pub(super) fn json_delete_path(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "json_delete_path takes 2 args".into(),
        ));
    }
    let mut target = value_to_json(&args[0]);
    let path = path_arg(&args[1])?;
    delete_path(&mut target, &path);
    typed_json_value(&target, true)
}

fn path_arg(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::Array(array) => Ok(array_strings(array.elements())),
        Value::List(items) => Ok(array_strings(items)),
        Value::Str(s) => Ok(s
            .trim_matches(|c| c == '{' || c == '}')
            .split(',')
            .filter(|part| !part.is_empty())
            .map(|part| part.trim().to_string())
            .collect()),
        other => Err(SQLError::TypeMismatch(format!(
            "JSON path must be an array, got {other:?}"
        ))),
    }
}

fn array_strings(values: &[Value]) -> Vec<String> {
    fn append(values: &[Value], output: &mut Vec<String>) {
        for value in values {
            if let Value::List(nested) = value {
                append(nested, output);
            } else {
                output.push(value_to_string(value));
            }
        }
    }

    let mut output = Vec::new();
    append(values, &mut output);
    output
}

fn delete_key_or_string(target: &mut serde_json::Value, key: &str) {
    match target {
        serde_json::Value::Object(map) => {
            map.remove(key);
        }
        serde_json::Value::Array(items) => {
            items.retain(|item| !matches!(item, serde_json::Value::String(value) if value == key));
        }
        _ => {}
    }
}

fn delete_array_index(target: &mut serde_json::Value, index: i64) {
    let serde_json::Value::Array(items) = target else {
        return;
    };
    let normalized = if index < 0 {
        items.len() as i64 + index
    } else {
        index
    };
    if let Ok(index) = usize::try_from(normalized) {
        if index < items.len() {
            items.remove(index);
        }
    }
}

fn delete_path(target: &mut serde_json::Value, path: &[String]) {
    let Some((head, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        match target {
            serde_json::Value::Object(map) => {
                map.remove(head);
            }
            serde_json::Value::Array(items) => {
                if let Some(index) = json_array_index(items.len(), head) {
                    items.remove(index);
                }
            }
            _ => {}
        }
        return;
    }
    match target {
        serde_json::Value::Object(map) => {
            if let Some(next) = map.get_mut(head) {
                delete_path(next, rest);
            }
        }
        serde_json::Value::Array(items) => {
            if let Some(index) = json_array_index(items.len(), head) {
                delete_path(&mut items[index], rest);
            }
        }
        _ => {}
    }
}

pub(super) fn jsonb_set(args: &[Value]) -> Result<Value> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::TypeMismatch("jsonb_set takes 3-4 args".into()));
    }
    let mut current = parse_json(&value_to_string(&args[0]))?;
    let path = path_arg(&args[1])?;
    let new_val = parse_json(&value_to_string(&args[2]))
        .unwrap_or_else(|_| serde_json::Value::String(value_to_string(&args[2])));
    let create_missing = args.get(3).is_none_or(|value| match value {
        Value::Bool(value) => *value,
        Value::Null => false,
        other => value_to_string(other).eq_ignore_ascii_case("true"),
    });
    json_set_path(&mut current, &path, new_val, create_missing);
    typed_json_value(&current, true)
}

pub(super) fn jsonb_insert(args: &[Value]) -> Result<Value> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::TypeMismatch("jsonb_insert takes 3-4 args".into()));
    }
    let mut current = parse_json(&value_to_string(&args[0]))?;
    let path = path_arg(&args[1])?;
    let new_val = parse_json(&value_to_string(&args[2]))
        .unwrap_or_else(|_| serde_json::Value::String(value_to_string(&args[2])));
    let insert_after = args.get(3).is_some_and(|value| match value {
        Value::Bool(value) => *value,
        other => value_to_string(other).eq_ignore_ascii_case("true"),
    });
    json_insert_path(&mut current, &path, new_val, insert_after);
    typed_json_value(&current, true)
}

fn json_insert_path(
    current: &mut serde_json::Value,
    path: &[String],
    new_val: serde_json::Value,
    insert_after: bool,
) -> bool {
    let Some((head, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return match current {
            serde_json::Value::Object(map) => {
                if map.contains_key(head) {
                    false
                } else {
                    map.insert(head.clone(), new_val);
                    true
                }
            }
            serde_json::Value::Array(items) => {
                let Some(index) = json_insert_index(items.len(), head, insert_after) else {
                    return false;
                };
                items.insert(index, new_val);
                true
            }
            _ => false,
        };
    }
    match current {
        serde_json::Value::Object(map) => map
            .get_mut(head)
            .is_some_and(|next| json_insert_path(next, rest, new_val, insert_after)),
        serde_json::Value::Array(items) => json_array_index(items.len(), head)
            .is_some_and(|index| json_insert_path(&mut items[index], rest, new_val, insert_after)),
        _ => false,
    }
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

fn json_set_path(
    current: &mut serde_json::Value,
    path: &[String],
    new_val: serde_json::Value,
    create_missing: bool,
) -> bool {
    if path.is_empty() {
        *current = new_val;
        return true;
    }
    let head = &path[0];
    let rest = &path[1..];
    match current {
        serde_json::Value::Object(obj) => {
            if !obj.contains_key(head) && !create_missing {
                return false;
            }
            let entry = obj.entry(head.clone()).or_insert(serde_json::Value::Null);
            json_set_path(entry, rest, new_val, create_missing)
        }
        serde_json::Value::Array(arr) => {
            if let Some(idx) = json_array_index(arr.len(), head) {
                json_set_path(&mut arr[idx], rest, new_val, create_missing)
            } else if create_missing && rest.is_empty() {
                if let Ok(idx) = head.parse::<usize>() {
                    while arr.len() <= idx {
                        arr.push(serde_json::Value::Null);
                    }
                    arr[idx] = new_val;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        }
        _ if create_missing => {
            let mut new_obj = serde_json::Map::new();
            new_obj.insert(head.clone(), serde_json::Value::Null);
            let mut wrapper = serde_json::Value::Object(new_obj);
            let changed = json_set_path(&mut wrapper, path, new_val, create_missing);
            if changed {
                *current = wrapper;
            }
            changed
        }
        _ => false,
    }
}
pub(super) fn strip_nulls(value: &mut serde_json::Value, strip_in_arrays: bool) {
    match value {
        serde_json::Value::Object(obj) => {
            obj.retain(|_, v| !v.is_null());
            for v in obj.values_mut() {
                strip_nulls(v, strip_in_arrays);
            }
        }
        serde_json::Value::Array(arr) => {
            if strip_in_arrays {
                arr.retain(|value| !value.is_null());
            }
            for v in arr.iter_mut() {
                strip_nulls(v, strip_in_arrays);
            }
        }
        _ => {}
    }
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
