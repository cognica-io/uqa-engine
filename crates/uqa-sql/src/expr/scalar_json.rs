//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON and `JSONPath` built-ins.

use super::{json, Result, SQLError, Value};
use uqa_core::memory::{Produced, ProductionControl};

pub(super) fn eval_json_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    if let Some(value) =
        eval_json_functions_with_control(name, args, &ProductionControl::uncontrolled())
    {
        return Some(value.map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary JSON evaluation has no lease")
        }));
    }
    // Polymorphic builders and set-returning key enumeration retain their existing ordinary paths. The generation validator rejects these functions.
    Some(match name {
        "json_build_object" | "jsonb_build_object" => {
            json::json_build_object_value(args, name.starts_with("jsonb"))
        }
        "json_build_array" | "jsonb_build_array" => {
            json::json_build_array_value(args, name.starts_with("jsonb"))
        }
        "to_json" | "to_jsonb" | "row_to_json" => to_json(name, args),
        "json_object_keys" | "jsonb_object_keys" => object_keys(args),
        _ => return None,
    })
}

pub(super) fn eval_json_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    json::evaluate(name, args, control)
}

fn to_json(name: &str, args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch("to_json takes 1 arg".into()));
    }
    let text = json::value_to_json_text(&args[0]);
    if name == "to_jsonb" {
        json::typed_json_value(&json::parse_json(&text)?, true)
    } else {
        Ok(Value::Json(text))
    }
}

fn object_keys(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch(
            "json_object_keys takes 1 arg".into(),
        ));
    }
    match json::parse_json(&super::value_to_string(&args[0])?)? {
        serde_json::Value::Object(map) => Ok(Value::List(
            map.into_iter().map(|(key, _)| Value::Str(key)).collect(),
        )),
        _ => Err(SQLError::TypeMismatch(
            "json_object_keys: argument is not an object".into(),
        )),
    }
}

#[cfg(test)]
mod tests;
