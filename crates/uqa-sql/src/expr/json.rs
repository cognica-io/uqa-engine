//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON scalar-function helpers for the expression evaluator.

use uqa_core::{TemporalValue, Value};

use crate::error::{Result, SQLError};

use super::{hex_encode, value_to_string};

pub(super) fn parse_json(s: &str) -> Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(s)
        .map_err(|e| SQLError::TypeMismatch(format!("invalid JSON: {e}")))
}

pub(super) fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => {
            // Try to parse strings already containing JSON; otherwise
            // wrap as a JSON string. Matches UQA behavior for `to_json` semantics
            // for nested expressions.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                if matches!(
                    parsed,
                    serde_json::Value::Object(_) | serde_json::Value::Array(_)
                ) {
                    return parsed;
                }
            }
            serde_json::Value::String(s.clone())
        }
        Value::Bytes(b) => serde_json::Value::String(format!("0x{}", hex_encode(b))),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
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

pub(super) fn json_build_object(args: &[Value]) -> Result<Value> {
    if args.len() % 2 != 0 {
        return Err(SQLError::TypeMismatch(
            "json_build_object requires an even number of args".into(),
        ));
    }
    let mut obj = serde_json::Map::new();
    for chunk in args.chunks_exact(2) {
        let key = value_to_string(&chunk[0]);
        let val = value_to_json(&chunk[1]);
        obj.insert(key, val);
    }
    Ok(json_to_value(&serde_json::Value::Object(obj)))
}

pub(super) fn json_build_array(args: &[Value]) -> Value {
    let homogeneous_numeric = args
        .iter()
        .all(|v| matches!(v, Value::Int(_) | Value::Float(_)));
    if homogeneous_numeric {
        Value::List(args.to_vec())
    } else {
        Value::List(
            args.iter()
                .map(|v| Value::Str(value_to_string(v)))
                .collect(),
        )
    }
}

pub(super) fn json_extract_path(args: &[Value], as_text: bool) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "json_extract_path takes 2+ args".into(),
        ));
    }
    let mut current = parse_json(&value_to_string(&args[0]))?;
    for key in &args[1..] {
        let key_str = value_to_string(key);
        current = match current {
            serde_json::Value::Object(mut obj) => {
                obj.remove(&key_str).unwrap_or(serde_json::Value::Null)
            }
            serde_json::Value::Array(arr) => match key_str.parse::<usize>() {
                Ok(idx) => arr.into_iter().nth(idx).unwrap_or(serde_json::Value::Null),
                Err(_) => serde_json::Value::Null,
            },
            _ => serde_json::Value::Null,
        };
    }
    if as_text {
        Ok(Value::Str(match current {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => return Ok(Value::Null),
            other => serde_json::to_string(&other).unwrap_or_default(),
        }))
    } else if matches!(current, serde_json::Value::Null) {
        Ok(Value::Null)
    } else {
        Ok(json_to_value(&current))
    }
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
    match (lhs, rhs) {
        (serde_json::Value::Object(l), serde_json::Value::Object(r)) => r
            .iter()
            .all(|(k, rv)| l.get(k).is_some_and(|lv| json_contains_value(lv, rv))),
        (serde_json::Value::Array(l), serde_json::Value::Array(r)) => r
            .iter()
            .all(|rv| l.iter().any(|lv| json_contains_value(lv, rv))),
        _ => lhs == rhs,
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
        _ => false,
    }))
}

pub(super) fn json_has_keys(args: &[Value], require_all: bool) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_has_keys takes 2 args".into()));
    }
    let obj = parse_json(&value_to_string(&args[0]))?;
    let keys = match &args[1] {
        Value::List(items) => items.iter().map(value_to_string).collect::<Vec<_>>(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "json key list must be array, got {other:?}"
            )));
        }
    };
    let serde_json::Value::Object(map) = obj else {
        return Ok(Value::Bool(false));
    };
    let found = |key: &String| map.contains_key(key);
    Ok(Value::Bool(if require_all {
        keys.iter().all(found)
    } else {
        keys.iter().any(found)
    }))
}

pub(super) fn jsonb_set(args: &[Value]) -> Result<Value> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::TypeMismatch("jsonb_set takes 3-4 args".into()));
    }
    let mut current = parse_json(&value_to_string(&args[0]))?;
    let path: Vec<String> = match &args[1] {
        Value::List(items) => items.iter().map(value_to_string).collect(),
        Value::Str(s) => {
            // Accept "{a,b,c}" PostgreSQL array literal.
            s.trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .map(|s| s.trim().to_string())
                .collect()
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "jsonb_set: path must be array, got {other:?}"
            )));
        }
    };
    let new_val = parse_json(&value_to_string(&args[2]))
        .unwrap_or_else(|_| serde_json::Value::String(value_to_string(&args[2])));
    json_set_path(&mut current, &path, new_val);
    Ok(json_to_value(&current))
}

fn json_set_path(current: &mut serde_json::Value, path: &[String], new_val: serde_json::Value) {
    if path.is_empty() {
        *current = new_val;
        return;
    }
    let head = &path[0];
    let rest = &path[1..];
    match current {
        serde_json::Value::Object(obj) => {
            let entry = obj.entry(head.clone()).or_insert(serde_json::Value::Null);
            json_set_path(entry, rest, new_val);
        }
        serde_json::Value::Array(arr) => {
            if let Ok(idx) = head.parse::<usize>() {
                while arr.len() <= idx {
                    arr.push(serde_json::Value::Null);
                }
                json_set_path(&mut arr[idx], rest, new_val);
            }
        }
        _ => {
            let mut new_obj = serde_json::Map::new();
            new_obj.insert(head.clone(), serde_json::Value::Null);
            let mut wrapper = serde_json::Value::Object(new_obj);
            json_set_path(&mut wrapper, path, new_val);
            *current = wrapper;
        }
    }
}

pub(super) fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(obj) => {
            obj.retain(|_, v| !v.is_null());
            for v in obj.values_mut() {
                strip_nulls(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_nulls(v);
            }
        }
        _ => {}
    }
}
