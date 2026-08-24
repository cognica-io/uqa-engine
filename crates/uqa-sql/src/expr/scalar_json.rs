//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON and `JSONPath` built-ins.

use super::{
    format_jsonb_pretty, json_build_array_value, json_build_object_value, json_contained_by,
    json_contains, json_delete_path, json_extract_path, json_has_key, json_has_keys, json_typeof,
    jsonb_insert, jsonb_set, jsonpath_exists, jsonpath_match, parse_json, strip_json_nulls_text,
    strip_nulls, typed_json_value, value_to_json_text, value_to_string, Result, SQLError, Value,
};

pub(super) fn eval_json_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "json_build_object",
        "jsonb_build_object",
        "json_build_array",
        "jsonb_build_array",
        "json_typeof",
        "jsonb_typeof",
        "json_array_length",
        "jsonb_array_length",
        "json_extract_path",
        "jsonb_extract_path",
        "json_extract_path_text",
        "jsonb_extract_path_text",
        "json_contains",
        "json_contained_by",
        "json_delete_path",
        "json_has_key",
        "json_has_any_key",
        "json_has_all_keys",
        "jsonb_path_exists",
        "jsonpath_exists",
        "jsonb_path_match",
        "jsonpath_match",
        "to_json",
        "to_jsonb",
        "row_to_json",
        "jsonb_set",
        "jsonb_insert",
        "jsonb_pretty",
        "json_strip_nulls",
        "jsonb_strip_nulls",
        "json_object_keys",
        "jsonb_object_keys",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // JSON functions
            // -------------------------------------------------------------
            "json_build_object" | "jsonb_build_object" => {
                json_build_object_value(args, name.starts_with("jsonb"))
            }
            "json_build_array" | "jsonb_build_array" => {
                json_build_array_value(args, name.starts_with("jsonb"))
            }
            "json_typeof" | "jsonb_typeof" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("json_typeof takes 1 arg".into()));
                }
                // Casts materialize jsonb scalars as engine values, so type
                // the value directly; only bare strings re-parse (and an
                // unparsable string IS a JSON string).
                let type_name = match &args[0] {
                    Value::Null => "null",
                    Value::Bool(_) => "boolean",
                    Value::Int(_) | Value::Float(_) | Value::Decimal(_) => "number",
                    Value::Json(text) | Value::JsonB(text) => json_typeof(&parse_json(text)?),
                    Value::List(_) => "array",
                    Value::Map(_) => "object",
                    Value::Str(s) => match parse_json(s) {
                        Ok(parsed) => json_typeof(&parsed),
                        Err(_) => "string",
                    },
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "json_typeof: unsupported {other:?}"
                        )));
                    }
                };
                Ok(Value::Str(type_name.to_string()))
            }
            "json_array_length" | "jsonb_array_length" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch(
                        "json_array_length takes 1 arg".into(),
                    ));
                }
                let parsed = parse_json(&value_to_string(&args[0]))?;
                match parsed {
                    serde_json::Value::Array(arr) => Ok(Value::Int(arr.len() as i64)),
                    _ => Err(SQLError::TypeMismatch(
                        "json_array_length: argument is not an array".into(),
                    )),
                }
            }
            "json_extract_path" | "jsonb_extract_path" => {
                json_extract_path(args, false, name.starts_with("jsonb"))
            }
            "json_extract_path_text" | "jsonb_extract_path_text" => {
                json_extract_path(args, true, name.starts_with("jsonb"))
            }
            "json_contains" => json_contains(args),
            "json_contained_by" => json_contained_by(args),
            "json_delete_path" => json_delete_path(args),
            "json_has_key" => json_has_key(args),
            "json_has_any_key" => json_has_keys(args, false),
            "json_has_all_keys" => json_has_keys(args, true),
            "jsonb_path_exists" | "jsonpath_exists" => jsonpath_exists(args),
            "jsonb_path_match" | "jsonpath_match" => jsonpath_match(args),
            "to_json" | "to_jsonb" | "row_to_json" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("to_json takes 1 arg".into()));
                }
                let text = value_to_json_text(&args[0]);
                if name == "to_jsonb" {
                    typed_json_value(&parse_json(&text)?, true)
                } else {
                    Ok(Value::Json(text))
                }
            }
            "jsonb_set" => jsonb_set(args),
            "jsonb_insert" => jsonb_insert(args),
            "jsonb_pretty" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("jsonb_pretty takes 1 arg".into()));
                }
                let parsed = parse_json(&value_to_string(&args[0]))?;
                Ok(Value::Str(format_jsonb_pretty(&parsed)))
            }
            "json_strip_nulls" | "jsonb_strip_nulls" => {
                if !(1..=2).contains(&args.len()) {
                    return Err(SQLError::TypeMismatch(
                        "json_strip_nulls takes 1 or 2 args".into(),
                    ));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let strip_in_arrays = match args.get(1) {
                    None => false,
                    Some(Value::Bool(value)) => *value,
                    Some(other) => {
                        return Err(SQLError::TypeMismatch(format!(
                            "json_strip_nulls: strip_in_arrays must be boolean, got {other:?}"
                        )));
                    }
                };
                let input = value_to_string(&args[0]);
                if name.starts_with("jsonb") {
                    let mut parsed = parse_json(&input)?;
                    strip_nulls(&mut parsed, strip_in_arrays);
                    typed_json_value(&parsed, true)
                } else {
                    Ok(Value::Json(strip_json_nulls_text(&input, strip_in_arrays)?))
                }
            }
            "json_object_keys" | "jsonb_object_keys" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch(
                        "json_object_keys takes 1 arg".into(),
                    ));
                }
                let parsed = parse_json(&value_to_string(&args[0]))?;
                match parsed {
                    serde_json::Value::Object(map) => Ok(Value::List(
                        map.keys().map(|k| Value::Str(k.clone())).collect(),
                    )),
                    _ => Err(SQLError::TypeMismatch(
                        "json_object_keys: argument is not an object".into(),
                    )),
                }
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}
