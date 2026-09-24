//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    access, field_index, from_value, input, insert_field, keys_equal, normalize_fields, parsed,
    typed, Field, Node, Values,
};
use crate::error::{Result, SQLError};
use crate::expr::conversion::value_to_string_with_control;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(in crate::expr) fn json_concat_with_control(
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_concat takes 2 args".into()));
    }
    if !args.iter().any(|arg| matches!(arg, Value::JsonB(_))) {
        return Ok(None);
    }
    control.check()?;
    let left = from_value(&args[0], false, control)?;
    let right = from_value(&args[1], false, control)?;
    let output = match (left, right) {
        (Node::Object(mut left), Node::Object(right)) => {
            for (ordinal, field) in left.as_mut_slice().iter_mut().enumerate() {
                control.check()?;
                field.ordinal = ordinal;
            }
            for mut field in right.into_owned_iter() {
                field.ordinal = left.len();
                left.push(field, control)?;
            }
            normalize_fields(&mut left, control)?;
            Node::Object(left)
        }
        (Node::Array(mut left), Node::Array(right)) => {
            for value in right.into_owned_iter() {
                left.push(value, control)?;
            }
            Node::Array(left)
        }
        (Node::Array(mut left), right) => {
            left.push(right, control)?;
            Node::Array(left)
        }
        (left, Node::Array(mut right)) => {
            right.insert(0, left, control)?;
            Node::Array(right)
        }
        (left, right) => {
            let mut values = Values::new(control);
            values.push(left, control)?;
            values.push(right, control)?;
            Node::Array(values)
        }
    };
    typed(&output, true, control).map(Some)
}

#[cfg(test)]
pub(in crate::expr) fn json_delete_with_control(
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("json_delete takes 2 args".into()));
    }
    json_delete_values_with_control(&args[0], &args[1], control)
}

pub(in crate::expr) fn json_delete_values_with_control(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if !matches!(*left, Value::JsonB(_) | Value::Map(_) | Value::List(_)) {
        return Ok(None);
    }
    let mut target = from_value(left, false, control)?;
    match right {
        Value::Int(index) => {
            if let Node::Array(values) = &mut target {
                let normalized = if *index < 0 {
                    values.len() as i64 + index
                } else {
                    *index
                };
                if let Ok(index) = usize::try_from(normalized) {
                    if index < values.len() {
                        values.remove(index, control)?;
                    }
                }
            }
        }
        Value::Array(array) => delete_keys(&mut target, array.elements(), control)?,
        Value::List(values) => delete_keys(&mut target, values, control)?,
        key => delete_key(
            &mut target,
            &value_to_string_with_control(key, control)?,
            control,
        )?,
    }
    typed(&target, true, control).map(Some)
}

fn delete_keys(target: &mut Node, keys: &[Value], control: &ProductionControl<'_>) -> Result<()> {
    let keys = access::strings(keys, control)?;
    for key in keys.iter() {
        delete_key(target, key, control)?;
    }
    Ok(())
}

fn delete_key(target: &mut Node, key: &str, control: &ProductionControl<'_>) -> Result<()> {
    control.check()?;
    match target {
        Node::Object(fields) => {
            if let Some(index) = field_index(fields, key, control)? {
                fields.remove(index, control)?;
            }
        }
        Node::Array(values) => values.retain(control, |value| match value {
            Node::String(value) => Ok(!keys_equal(value, key, control)?),
            _ => Ok(true),
        })?,
        _ => {}
    }
    Ok(())
}

pub(super) fn delete(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "json_delete_path takes 2 args".into(),
        ));
    }
    let mut target = from_value(&args[0], false, control)?;
    delete_path(&mut target, &access::path_arg(&args[1], control)?, control)?;
    typed(&target, true, control)
}

fn delete_path(target: &mut Node, path: &[String], control: &ProductionControl<'_>) -> Result<()> {
    control.check()?;
    let Some((head, rest)) = path.split_first() else {
        return Ok(());
    };
    if rest.is_empty() {
        match target {
            Node::Object(fields) => {
                if let Some(index) = field_index(fields, head, control)? {
                    fields.remove(index, control)?;
                }
            }
            Node::Array(values) => {
                if let Some(index) = super::super::json_array_index(values.len(), head) {
                    values.remove(index, control)?;
                }
            }
            _ => {}
        }
    } else if let Some(next) = child_mut(target, head, control)? {
        delete_path(next, rest, control)?;
    }
    Ok(())
}

fn child_mut<'a>(
    target: &'a mut Node,
    key: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a mut Node>> {
    control.check()?;
    Ok(match target {
        Node::Object(fields) => {
            field_index(fields, key, control)?.map(|index| &mut fields.as_mut_slice()[index].value)
        }
        Node::Array(values) => super::super::json_array_index(values.len(), key)
            .map(|index| &mut values.as_mut_slice()[index]),
        _ => None,
    })
}

pub(super) fn set_or_insert(
    args: &[Value],
    insert: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            if insert {
                "jsonb_insert takes 3-4 args"
            } else {
                "jsonb_set takes 3-4 args"
            }
            .into(),
        ));
    }
    let mut current = input(&args[0], control)?;
    let path = access::path_arg(&args[1], control)?;
    let text = value_to_string_with_control(&args[2], control)?;
    let new_value = match parsed::parse_optional(&text, control)? {
        Some(value) => value,
        None => Node::String(text),
    };
    let flag = match args.get(3) {
        None => !insert,
        Some(Value::Bool(value)) => *value,
        Some(Value::Null) if !insert => false,
        Some(other) => value_to_string_with_control(other, control)?.eq_ignore_ascii_case("true"),
    };
    if insert {
        insert_path(&mut current, &path, new_value, flag, control)?;
    } else {
        set_path(&mut current, &path, new_value, flag, control)?;
    }
    typed(&current, true, control)
}

fn insert_path(
    current: &mut Node,
    path: &[String],
    value: Node,
    after: bool,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    control.check()?;
    let Some((head, rest)) = path.split_first() else {
        return Ok(false);
    };
    if !rest.is_empty() {
        return match child_mut(current, head, control)? {
            Some(next) => insert_path(next, rest, value, after, control),
            None => Ok(false),
        };
    }
    match current {
        Node::Object(fields) => {
            if field_index(fields, head, control)?.is_some() {
                return Ok(false);
            }
            let ordinal = fields.len();
            insert_field(
                fields,
                Field {
                    key: control.copy_text(head)?,
                    value,
                    ordinal,
                },
                control,
            )?;
            Ok(true)
        }
        Node::Array(values) => {
            let Some(index) = super::super::json_insert_index(values.len(), head, after) else {
                return Ok(false);
            };
            values.insert(index, value, control)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn set_path(
    current: &mut Node,
    path: &[String],
    value: Node,
    create: bool,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    control.check()?;
    let Some((head, rest)) = path.split_first() else {
        *current = value;
        return Ok(true);
    };
    match current {
        Node::Object(fields) => {
            let index = if let Some(index) = field_index(fields, head, control)? {
                index
            } else if create {
                let ordinal = fields.len();
                insert_field(
                    fields,
                    Field {
                        key: control.copy_text(head)?,
                        value: Node::Null,
                        ordinal,
                    },
                    control,
                )?
            } else {
                return Ok(false);
            };
            set_path(
                &mut fields.as_mut_slice()[index].value,
                rest,
                value,
                create,
                control,
            )
        }
        Node::Array(values) => {
            if let Some(index) = super::super::json_array_index(values.len(), head) {
                return set_path(
                    &mut values.as_mut_slice()[index],
                    rest,
                    value,
                    create,
                    control,
                );
            }
            if create && rest.is_empty() {
                if let Ok(index) = head.parse::<usize>() {
                    while values.len() <= index {
                        values.push(Node::Null, control)?;
                    }
                    values.as_mut_slice()[index] = value;
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ if create => {
            let mut fields = Values::new(control);
            fields.push(
                Field {
                    key: control.copy_text(head)?,
                    value: Node::Null,
                    ordinal: fields.len(),
                },
                control,
            )?;
            let mut wrapper = Node::Object(fields);
            let changed = set_path(&mut wrapper, path, value, create, control)?;
            if changed {
                *current = wrapper;
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

pub(super) fn strip(node: &mut Node, arrays: bool, control: &ProductionControl<'_>) -> Result<()> {
    control.check()?;
    match node {
        Node::Object(fields) => {
            fields.retain(control, |field| Ok(!matches!(field.value, Node::Null)))?;
            for field in fields.as_mut_slice() {
                strip(&mut field.value, arrays, control)?;
            }
        }
        Node::Array(values) => {
            if arrays {
                values.retain(control, |value| Ok(!matches!(value, Node::Null)))?;
            }
            for value in values.as_mut_slice() {
                strip(value, arrays, control)?;
            }
        }
        _ => {}
    }
    Ok(())
}
