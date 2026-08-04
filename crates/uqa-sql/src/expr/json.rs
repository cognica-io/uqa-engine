//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON scalar-function helpers for the expression evaluator.

use uqa_core::{DecimalValue, TemporalValue, Value};

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
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(d.to_sql_string())),
        Value::Str(s) => {
            // Parse strings already containing JSON; otherwise wrap them as a
            // JSON string for nested `to_json` expressions.
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

pub(super) fn json_build_object(args: &[Value]) -> Result<Value> {
    if !args.len().is_multiple_of(2) {
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
            other => other.to_string(),
        }))
    } else if matches!(current, serde_json::Value::Null) {
        Ok(Value::Null)
    } else {
        Ok(json_to_value(&current))
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
    match (lhs, rhs) {
        (serde_json::Value::Object(l), serde_json::Value::Object(r)) => r
            .iter()
            .all(|(k, rv)| l.get(k).is_some_and(|lv| json_contains_value(lv, rv))),
        (serde_json::Value::Array(l), serde_json::Value::Array(r)) => r
            .iter()
            .all(|rv| l.iter().any(|lv| json_contains_value(lv, rv))),
        (serde_json::Value::Array(l), r) => l.iter().any(|lv| json_contains_value(lv, r)),
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
        Value::List(items) => items.iter().map(value_to_string).collect::<Vec<_>>(),
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

pub(super) fn jsonpath_exists(args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "jsonpath_exists takes at least 2 args".into(),
        ));
    }
    let root = parse_json(&value_to_string(&args[0]))?;
    let path = value_to_string(&args[1]);
    Ok(Value::Bool(jsonpath_exists_value(&root, &path)?))
}

pub(super) fn jsonpath_match(args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "jsonpath_match takes at least 2 args".into(),
        ));
    }
    let root = parse_json(&value_to_string(&args[0]))?;
    let path = value_to_string(&args[1]);
    Ok(Value::Bool(jsonpath_match_value(&root, &path)?))
}

pub(super) fn jsonpath_candidate(args: &[Value]) -> bool {
    matches!(args.get(1), Some(Value::Str(path)) if path.trim_start().starts_with('$'))
        && matches!(args.first(), Some(Value::Map(_) | Value::List(_)))
}

fn jsonpath_exists_value(root: &serde_json::Value, path: &str) -> Result<bool> {
    let path = normalize_jsonpath(path);
    let (selector, filter) = split_jsonpath_filter(&path);
    let values = jsonpath_select(root, selector)?;
    let Some(filter) = filter else {
        return Ok(!values.is_empty());
    };
    for value in values {
        if eval_jsonpath_predicate(root, &value, filter)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn jsonpath_match_value(root: &serde_json::Value, path: &str) -> Result<bool> {
    let path = normalize_jsonpath(path);
    if let Some((left, op, right)) = split_jsonpath_comparison(&path) {
        let values = if left == "@" {
            vec![root.clone()]
        } else {
            jsonpath_select(root, left)?
        };
        let rhs = parse_jsonpath_literal(right)?;
        return Ok(values
            .iter()
            .any(|value| compare_jsonpath_values(value, &rhs, op)));
    }
    let values = jsonpath_select(root, &path)?;
    Ok(values
        .first()
        .is_some_and(|value| matches!(value, serde_json::Value::Bool(true))))
}

fn normalize_jsonpath(path: &str) -> String {
    let path = path.trim();
    path.strip_prefix("strict ")
        .or_else(|| path.strip_prefix("lax "))
        .unwrap_or(path)
        .trim()
        .to_string()
}

fn split_jsonpath_filter(path: &str) -> (&str, Option<&str>) {
    let Some(pos) = path.find('?') else {
        return (path.trim(), None);
    };
    let selector = path[..pos].trim();
    let filter = path[pos + 1..].trim();
    (selector, Some(strip_wrapping_parens(filter)))
}

fn strip_wrapping_parens(input: &str) -> &str {
    let input = input.trim();
    if input.starts_with('(') && input.ends_with(')') {
        input[1..input.len() - 1].trim()
    } else {
        input
    }
}

fn jsonpath_select(root: &serde_json::Value, selector: &str) -> Result<Vec<serde_json::Value>> {
    let mut rest = selector.trim();
    if rest == "$" {
        return Ok(vec![root.clone()]);
    }
    let Some(after_root) = rest.strip_prefix('$') else {
        return Err(SQLError::TypeMismatch(format!(
            "jsonpath selector must start with $, got {selector:?}"
        )));
    };
    rest = after_root;
    let mut current = vec![root.clone()];
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix('.') {
            let (key, after_key) = take_jsonpath_key(next)?;
            current = current
                .into_iter()
                .filter_map(|value| match value {
                    serde_json::Value::Object(map) => map.get(key).cloned(),
                    _ => None,
                })
                .collect();
            rest = after_key;
        } else if let Some(next) = rest.strip_prefix("[*]") {
            current = current
                .into_iter()
                .flat_map(|value| match value {
                    serde_json::Value::Array(items) => items,
                    _ => Vec::new(),
                })
                .collect();
            rest = next;
        } else if let Some(next) = rest.strip_prefix('[') {
            let Some(end) = next.find(']') else {
                return Err(SQLError::TypeMismatch(format!(
                    "unterminated jsonpath array index in {selector:?}"
                )));
            };
            let index = &next[..end];
            current = current
                .into_iter()
                .filter_map(|value| match value {
                    serde_json::Value::Array(items) => {
                        json_array_index(items.len(), index).and_then(|idx| items.get(idx).cloned())
                    }
                    _ => None,
                })
                .collect();
            rest = &next[end + 1..];
        } else {
            return Err(SQLError::TypeMismatch(format!(
                "unsupported jsonpath selector tail {rest:?}"
            )));
        }
    }
    Ok(current)
}

fn take_jsonpath_key(input: &str) -> Result<(&str, &str)> {
    if let Some(quoted) = input.strip_prefix('"') {
        let Some(end) = quoted.find('"') else {
            return Err(SQLError::TypeMismatch(
                "unterminated quoted jsonpath key".into(),
            ));
        };
        return Ok((&quoted[..end], &quoted[end + 1..]));
    }
    let end = input
        .find(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .unwrap_or(input.len());
    if end == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "expected jsonpath key in {input:?}"
        )));
    }
    Ok((&input[..end], &input[end..]))
}

fn eval_jsonpath_predicate(
    root: &serde_json::Value,
    current: &serde_json::Value,
    predicate: &str,
) -> Result<bool> {
    if let Some((left, op, right)) = split_jsonpath_comparison(predicate) {
        let values = if left == "@" {
            vec![current.clone()]
        } else if let Some(selector) = left.strip_prefix('@') {
            jsonpath_select(current, &format!("${selector}"))?
        } else {
            jsonpath_select(root, left)?
        };
        let rhs = parse_jsonpath_literal(right)?;
        return Ok(values
            .iter()
            .any(|value| compare_jsonpath_values(value, &rhs, op)));
    }
    let values = if predicate == "@" {
        vec![current.clone()]
    } else {
        jsonpath_select(root, predicate)?
    };
    Ok(!values.is_empty())
}

fn split_jsonpath_comparison(input: &str) -> Option<(&str, &'static str, &str)> {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some(pos) = input.find(op) {
            return Some((input[..pos].trim(), op, input[pos + op.len()..].trim()));
        }
    }
    None
}

fn parse_jsonpath_literal(input: &str) -> Result<serde_json::Value> {
    let input = input.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
        return Ok(value);
    }
    if input.starts_with('"') && input.ends_with('"') {
        return Ok(serde_json::Value::String(
            input[1..input.len() - 1].to_string(),
        ));
    }
    Err(SQLError::TypeMismatch(format!(
        "unsupported jsonpath literal {input:?}"
    )))
}

fn compare_jsonpath_values(lhs: &serde_json::Value, rhs: &serde_json::Value, op: &str) -> bool {
    match (lhs, rhs) {
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => {
            let Some(left) = left.as_f64() else {
                return false;
            };
            let Some(right) = right.as_f64() else {
                return false;
            };
            compare_f64(left, right, op)
        }
        (serde_json::Value::String(left), serde_json::Value::String(right)) => {
            compare_ordering(left.cmp(right), op)
        }
        (serde_json::Value::Bool(left), serde_json::Value::Bool(right)) => {
            compare_ordering(left.cmp(right), op)
        }
        (serde_json::Value::Null, serde_json::Value::Null) => matches!(op, "==" | ">=" | "<="),
        _ => matches!(op, "!="),
    }
}

fn compare_f64(left: f64, right: f64, op: &str) -> bool {
    match op {
        "==" => left == right,
        "!=" => left != right,
        ">" => left > right,
        ">=" => left >= right,
        "<" => left < right,
        "<=" => left <= right,
        _ => false,
    }
}

fn compare_ordering(ordering: std::cmp::Ordering, op: &str) -> bool {
    match op {
        "==" => ordering.is_eq(),
        "!=" => !ordering.is_eq(),
        ">" => ordering.is_gt(),
        ">=" => ordering.is_ge(),
        "<" => ordering.is_lt(),
        "<=" => ordering.is_le(),
        _ => false,
    }
}

pub(super) fn json_concat(args: &[Value]) -> Result<Option<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_concat takes 2 args".into()));
    }
    if !args
        .iter()
        .any(|arg| matches!(arg, Value::Map(_) | Value::List(_)))
    {
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
    Ok(Some(json_to_value(&out)))
}

pub(super) fn json_delete(args: &[Value]) -> Result<Option<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_delete takes 2 args".into()));
    }
    if !matches!(args[0], Value::Map(_) | Value::List(_)) {
        return Ok(None);
    }
    let mut target = value_to_json(&args[0]);
    match &args[1] {
        Value::Int(index) => delete_array_index(&mut target, *index),
        Value::List(keys) => {
            for key in keys {
                delete_key_or_string(&mut target, &value_to_string(key));
            }
        }
        key => delete_key_or_string(&mut target, &value_to_string(key)),
    }
    Ok(Some(json_to_value(&target)))
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
    Ok(json_to_value(&target))
}

fn path_arg(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::List(items) => Ok(items.iter().map(value_to_string).collect()),
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
    Ok(json_to_value(&current))
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
    Ok(json_to_value(&current))
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
