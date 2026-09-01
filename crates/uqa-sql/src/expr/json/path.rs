//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{json_array_index, parse_json, value_to_string, Result, SQLError, Value};

pub(in crate::expr) fn jsonpath_exists(args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "jsonpath_exists takes at least 2 args".into(),
        ));
    }
    let root = parse_json(&value_to_string(&args[0]))?;
    let path = value_to_string(&args[1]);
    Ok(Value::Bool(jsonpath_exists_value(&root, &path)?))
}

pub(in crate::expr) fn jsonpath_match(args: &[Value]) -> Result<Value> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "jsonpath_match takes at least 2 args".into(),
        ));
    }
    let root = parse_json(&value_to_string(&args[0]))?;
    let path = value_to_string(&args[1]);
    Ok(Value::Bool(jsonpath_match_value(&root, &path)?))
}

pub(in crate::expr) fn jsonpath_candidate(args: &[Value]) -> bool {
    matches!(args.get(1), Some(Value::Str(path)) if path.trim_start().starts_with('$'))
        && matches!(
            args.first(),
            Some(Value::Json(_) | Value::JsonB(_) | Value::Map(_) | Value::List(_))
        )
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
