//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable JSON scalar dispatch preserves the ordinary arity, NULL and error order.

use super::{access, inline, input, jsonpath, mutation, parsed, pretty, text_value, typed, Node};
use crate::error::{Result, SQLError};
use crate::expr::{
    conversion::value_to_string_with_control, json_strip::strip_json_nulls_text_with_control,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(in crate::expr) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    Some(match name {
        "json_typeof" | "jsonb_typeof" => typeof_value(args, control),
        "json_array_length" | "jsonb_array_length" => array_length(args, control),
        "json_extract_path" | "jsonb_extract_path" => {
            access::extract_path(args, false, name.starts_with("jsonb"), control)
        }
        "json_extract_path_text" | "jsonb_extract_path_text" => {
            access::extract_path(args, true, name.starts_with("jsonb"), control)
        }
        "json_contains" => access::contains(args, false, control),
        "json_contained_by" => access::contains(args, true, control),
        "json_delete_path" => mutation::delete(args, control),
        "json_has_key" => has_key(args, control),
        "json_has_any_key" => has_keys(args, false, control),
        "json_has_all_keys" => has_keys(args, true, control),
        "jsonb_path_exists" | "jsonpath_exists" => jsonpath::evaluate(args, false, control),
        "jsonb_path_match" | "jsonpath_match" => jsonpath::evaluate(args, true, control),
        "jsonb_set" => mutation::set_or_insert(args, false, control),
        "jsonb_insert" => mutation::set_or_insert(args, true, control),
        "jsonb_pretty" => pretty_value(args, control),
        "json_strip_nulls" | "jsonb_strip_nulls" => strip(args, name.starts_with("jsonb"), control),
        _ => return None,
    })
}

fn typeof_value(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value] = args else {
        return Err(SQLError::TypeMismatch("json_typeof takes 1 arg".into()));
    };
    control.check()?;
    let name = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) | Value::Float(_) | Value::Decimal(_) => "number",
        Value::Json(text) | Value::JsonB(text) => type_name(&parsed::parse(text, control)?),
        Value::List(_) => "array",
        Value::Map(_) => "object",
        Value::Str(text) => parsed::parse_optional(text, control)?
            .as_ref()
            .map_or("string", type_name),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "json_typeof: unsupported {other:?}"
            )));
        }
    };
    text_value(control.copy_text(name)?, None, control)
}

fn type_name(node: &Node) -> &'static str {
    match node {
        Node::Null => "null",
        Node::Bool(_) => "boolean",
        Node::Number(_) => "number",
        Node::String(_) => "string",
        Node::Array(_) => "array",
        Node::Object(_) => "object",
    }
}

fn array_length(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value] = args else {
        return Err(SQLError::TypeMismatch(
            "json_array_length takes 1 arg".into(),
        ));
    };
    let Node::Array(values) = input(value, control)? else {
        return Err(SQLError::TypeMismatch(
            "json_array_length: argument is not an array".into(),
        ));
    };
    inline(
        Value::Int(i64::try_from(values.len()).expect("JSON array allocation length fits i64")),
        control,
    )
}

fn has_key(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value, key] = args else {
        return Err(SQLError::TypeMismatch("json_has_key takes 2 args".into()));
    };
    let node = input(value, control)?;
    let key = value_to_string_with_control(key, control)?;
    inline(Value::Bool(access::has_key(&node, &key, control)?), control)
}

fn has_keys(args: &[Value], all: bool, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value, keys] = args else {
        return Err(SQLError::TypeMismatch("json_has_keys takes 2 args".into()));
    };
    let node = input(value, control)?;
    let keys = match keys {
        Value::Array(array) => access::strings(array.elements(), control)?,
        Value::List(values) => access::strings(values, control)?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "json key list must be array, got {other:?}"
            )));
        }
    };
    for key in keys.iter() {
        if access::has_key(&node, key, control)? != all {
            return inline(Value::Bool(!all), control);
        }
    }
    inline(Value::Bool(all), control)
}

fn pretty_value(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value] = args else {
        return Err(SQLError::TypeMismatch("jsonb_pretty takes 1 arg".into()));
    };
    text_value(
        pretty::format(&input(value, control)?, control)?,
        None,
        control,
    )
}

fn strip(args: &[Value], jsonb: bool, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "json_strip_nulls takes 1 or 2 args".into(),
        ));
    }
    control.check()?;
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return inline(Value::Null, control);
    }
    let arrays = match args.get(1) {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(other) => {
            return Err(SQLError::TypeMismatch(format!(
                "json_strip_nulls: strip_in_arrays must be boolean, got {other:?}"
            )));
        }
    };
    let text = value_to_string_with_control(&args[0], control)?;
    if jsonb {
        let mut node = parsed::parse(&text, control)?;
        mutation::strip(&mut node, arrays, control)?;
        typed(&node, true, control)
    } else {
        text_value(
            strip_json_nulls_text_with_control(&text, arrays, control)?,
            Some(false),
            control,
        )
    }
}
