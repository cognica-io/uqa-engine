//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{field_index, inline, input, keys_equal, pretty, text_value, typed, writer, Node};
use crate::error::{Result, SQLError};
use crate::expr::conversion::value_to_string_with_control;
use uqa_core::jsonb_equality_key_with_control;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub(super) fn extract_path(
    args: &[Value],
    as_text: bool,
    jsonb: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.len() < 2 {
        return Err(SQLError::TypeMismatch(
            "json_extract_path takes 2+ args".into(),
        ));
    }
    extract(&args[0], &args[1..], as_text, jsonb, None, control)
}

pub(in crate::expr) fn json_extract_operator_with_control(
    args: &[Value],
    as_text: bool,
    path: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let [input, key] = args else {
        return Err(SQLError::Internal(
            "JSON extraction requires two operands".into(),
        ));
    };
    if matches!(input, Value::Null) || matches!(key, Value::Null) {
        return inline(Value::Null, control);
    }
    if path {
        let keys =
            crate::expr::casting::cast_value_from_with_control(key, "text[]", None, control)?;
        let Value::Array(keys) = &*keys else {
            return Err(SQLError::Internal(
                "JSON path cast must return an array".into(),
            ));
        };
        extract(input, keys.elements(), as_text, false, None, control)
    } else {
        extract(
            input,
            std::slice::from_ref(key),
            as_text,
            false,
            Some(matches!(key, Value::Int(_))),
            control,
        )
    }
}

fn extract(
    value: &Value,
    keys: &[Value],
    as_text: bool,
    jsonb: bool,
    array_index: Option<bool>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if matches!(value, Value::Null) || keys.iter().any(|key| matches!(key, Value::Null)) {
        return inline(Value::Null, control);
    }
    let jsonb = jsonb || matches!(value, Value::JsonB(_));
    let root = input(value, control)?;
    if matches!(
        (&root, array_index),
        (Node::Object(_), Some(true)) | (Node::Array(_), Some(false))
    ) {
        return inline(Value::Null, control);
    }
    let mut current = &root;
    for key in keys {
        let key = value_to_string_with_control(key, control)?;
        let Some(next) = child(current, &key, control)? else {
            return inline(Value::Null, control);
        };
        current = next;
    }
    if as_text {
        match current {
            Node::Null => inline(Value::Null, control),
            Node::String(text) => text_value(control.copy_text(text)?, None, control),
            other => text_value(pretty::compact(other, jsonb, control)?, None, control),
        }
    } else {
        typed(current, jsonb, control)
    }
}

pub(super) fn child<'a>(
    node: &'a Node,
    key: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a Node>> {
    control.check()?;
    Ok(match node {
        Node::Object(fields) => {
            field_index(fields, key, control)?.map(|index| &fields[index].value)
        }
        Node::Array(values) => {
            super::super::json_array_index(values.len(), key).map(|index| &values[index])
        }
        _ => None,
    })
}

pub(super) fn contains(
    args: &[Value],
    reverse: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            if reverse {
                "json_contained_by takes 2 args"
            } else {
                "json_contains takes 2 args"
            }
            .into(),
        ));
    }
    let left = input(&args[0], control)?;
    let right = input(&args[1], control)?;
    inline(
        Value::Bool(if reverse {
            contains_at(&right, &left, true, control)?
        } else {
            contains_at(&left, &right, true, control)?
        }),
        control,
    )
}

fn contains_at(
    left: &Node,
    right: &Node,
    top: bool,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    control.check()?;
    match (left, right) {
        (Node::Object(left), Node::Object(right)) => {
            for field in right.iter() {
                let Some(index) = field_index(left, &field.key, control)? else {
                    return Ok(false);
                };
                if !contains_at(&left[index].value, &field.value, false, control)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Node::Array(left), Node::Array(right)) => {
            for value in right.iter() {
                if !any_contains(left, value, control)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Node::Array(left), right) if top && !matches!(right, Node::Array(_) | Node::Object(_)) => {
            any_contains(left, right, control)
        }
        _ => {
            let left = writer::format(left, false, control)?;
            let right = writer::format(right, false, control)?;
            let left = jsonb_equality_key_with_control(&left, control)?;
            let right = jsonb_equality_key_with_control(&right, control)?;
            match (left, right) {
                (Some(left), Some(right)) => {
                    if left.len() != right.len() {
                        return Ok(false);
                    }
                    for (left, right) in left.chunks(4096).zip(right.chunks(4096)) {
                        control.check()?;
                        if left != right {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
    }
}

fn any_contains(left: &[Node], right: &Node, control: &ProductionControl<'_>) -> Result<bool> {
    for value in left {
        if contains_at(value, right, false, control)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn has_key(node: &Node, key: &str, control: &ProductionControl<'_>) -> Result<bool> {
    control.check()?;
    match node {
        Node::Object(fields) => Ok(field_index(fields, key, control)?.is_some()),
        Node::Array(values) => {
            for value in values.iter() {
                control.check()?;
                if let Node::String(value) = value {
                    if keys_equal(value, key, control)? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

pub(super) fn strings(
    values: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>> {
    fn append(
        values: &[Value],
        output: &mut ProductionVec<'_, String>,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        for value in values {
            control.check()?;
            if let Value::List(nested) = value {
                append(nested, output, control)?;
            } else {
                output.push_produced(value_to_string_with_control(value, control)?)?;
            }
        }
        Ok(())
    }
    let mut output = ProductionVec::new(*control);
    append(values, &mut output, control)?;
    Ok(output.finish()?)
}

pub(super) fn path_arg(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>> {
    match value {
        Value::Array(array) => strings(array.elements(), control),
        Value::List(values) => strings(values, control),
        Value::Str(text) => {
            let mut output = ProductionVec::new(*control);
            // Preserve the existing brace trimming, empty-component filtering and whitespace ordering.
            for part in text.trim_matches(['{', '}']).split(',') {
                control.check()?;
                if !part.is_empty() {
                    output.push_produced(control.copy_text(part.trim())?)?;
                }
            }
            Ok(output.finish()?)
        }
        other => Err(SQLError::TypeMismatch(format!(
            "JSON path must be an array, got {other:?}"
        ))),
    }
}
