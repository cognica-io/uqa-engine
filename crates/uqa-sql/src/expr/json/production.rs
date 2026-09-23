//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted JSON input carriers and output share the scalar serializer and JSON token grammar.

use super::{Result, Value};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

mod access;
mod functions;
mod jsonpath;
mod mutation;
mod parsed;
mod pretty;
mod values;
mod writer;
use values::Values;

/// Container capacities and their elements own separate leases. Dropping a replaced or temporary node releases all of its payloads without a process-wide cache.
enum Node {
    Null,
    Bool(bool),
    Number(Produced<String>),
    String(Produced<String>),
    Array(Values<Self>),
    Object(Values<Field>),
}

struct Field {
    key: Produced<String>,
    value: Node,
    ordinal: usize,
}

pub(in crate::expr) fn cast_json_value_with_control(
    value: &Value,
    jsonb: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if matches!(
        (jsonb, value),
        (false, Value::Json(_)) | (true, Value::JsonB(_))
    ) {
        return Ok(control.copy_value(value)?);
    }
    let node = match value {
        Value::Str(text) | Value::FixedChar(text) => {
            let parsed = parsed::parse(text, control)?;
            if !jsonb {
                let (text, memory) = control.copy_text(text)?.into_parts();
                return Ok(control.finish(Value::Json(text), memory)?);
            }
            parsed
        }
        Value::Json(text) if jsonb => parsed::parse(text, control)?,
        other => from_value(other, false, control)?,
    };
    let (text, memory) = writer::format(&node, jsonb, control)?.into_parts();
    Ok(control.finish(
        if jsonb {
            Value::JsonB(text)
        } else {
            Value::Json(text)
        },
        memory,
    )?)
}

pub(in crate::expr) fn format_value_as_json_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    writer::format(&from_value(value, false, control)?, false, control)
}

pub(in crate::expr) fn format_core_value_as_json_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    writer::format(&from_value(value, true, control)?, false, control)
}

pub(in crate::expr) fn utf8_lossy_with_control(
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    let mut start = 0;
    while start < bytes.len() {
        control.check()?;
        let end = start.saturating_add(4096).min(bytes.len());
        match std::str::from_utf8(&bytes[start..end]) {
            Ok(text) => {
                output.push_str(text)?;
                start = end;
            }
            Err(error) => {
                let valid_end = start + error.valid_up_to();
                output.push_str(
                    std::str::from_utf8(&bytes[start..valid_end]).expect("validated UTF-8 prefix"),
                )?;
                start = valid_end;
                if let Some(length) = error.error_len() {
                    output.push('�')?;
                    start += length;
                } else if end == bytes.len() {
                    output.push('�')?;
                    start = end;
                }
                // An incomplete code point at a chunk boundary is retried together with the next bytes.
            }
        }
    }
    Ok(output.finish()?)
}

fn from_value(value: &Value, core_carrier: bool, control: &ProductionControl<'_>) -> Result<Node> {
    control.check()?;
    Ok(match value {
        Value::Null => Node::Null,
        Value::Void => Node::String(control.copy_text("")?),
        Value::Bool(value) => Node::Bool(*value),
        Value::Int(value) => Node::Number(control.format(format_args!("{value}"))?),
        Value::Float(value) if value.is_finite() => Node::Number(writer::scalar(value, control)?),
        Value::Float(value) => Node::String(control.copy_text(if value.is_nan() {
            "NaN"
        } else if value.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        })?),
        Value::Decimal(value) if core_carrier => {
            let text = value.to_sql_string_with_control(control)?;
            if let Some(value) = text.parse::<f64>().ok().filter(|value| value.is_finite()) {
                Node::Number(writer::scalar(&value, control)?)
            } else {
                Node::String(text)
            }
        }
        Value::Decimal(value) => {
            let text = value.to_sql_string_with_control(control)?;
            if value.is_nan() || value.is_infinite() {
                Node::String(text)
            } else {
                Node::Number(text)
            }
        }
        Value::Str(value) if core_carrier => match parsed::parse_optional(value, control)? {
            Some(value) => value,
            None => Node::String(control.copy_text(value)?),
        },
        Value::Str(value) => Node::String(control.copy_text(value)?),
        Value::FixedChar(value) => Node::String(control.copy_text(value.trim_end_matches(' '))?),
        Value::Bytes(bytes) if core_carrier => {
            Node::String(utf8_lossy_with_control(bytes, control)?)
        }
        Value::Bytes(bytes) => {
            let mut text = ProductionString::new(*control);
            text.push_str("0x")?;
            const HEX: &[u8; 16] = b"0123456789abcdef";
            for byte in bytes {
                text.push(char::from(HEX[usize::from(byte >> 4)]))?;
                text.push(char::from(HEX[usize::from(byte & 15)]))?;
            }
            Node::String(text.finish()?)
        }
        Value::Temporal(value) => Node::String(value.to_sql_string_with_control(control)?),
        Value::Json(text) | Value::JsonB(text) => match parsed::parse_optional(text, control)? {
            Some(value) => value,
            None => Node::String(control.copy_text(text)?),
        },
        Value::Array(array) => array_node(array.elements(), core_carrier, control)?,
        Value::List(values) => array_node(values, core_carrier, control)?,
        Value::Row(values) => {
            let mut fields = Values::new(control);
            for (index, value) in values.iter().enumerate() {
                let key = control.format(format_args!("f{}", index + 1))?;
                fields.push(
                    Field {
                        key,
                        value: from_value(value, core_carrier, control)?,
                        ordinal: index,
                    },
                    control,
                )?;
            }
            normalize_fields(&mut fields, control)?;
            Node::Object(fields)
        }
        Value::Record(values) => object_node(
            values.iter().map(|(key, value)| (key.as_str(), value)),
            core_carrier,
            control,
        )?,
        Value::Map(values) => object_node(
            values.iter().map(|(key, value)| (key.as_str(), value)),
            core_carrier,
            control,
        )?,
    })
}

fn array_node(
    values: &[Value],
    core_carrier: bool,
    control: &ProductionControl<'_>,
) -> Result<Node> {
    let mut nodes = Values::new(control);
    for value in values {
        nodes.push(from_value(value, core_carrier, control)?, control)?;
    }
    Ok(Node::Array(nodes))
}

fn object_node<'a>(
    fields: impl IntoIterator<Item = (&'a str, &'a Value)>,
    core_carrier: bool,
    control: &ProductionControl<'_>,
) -> Result<Node> {
    let mut nodes = Values::new(control);
    for (ordinal, (key, value)) in fields.into_iter().enumerate() {
        let field = Field {
            key: control.copy_text(key)?,
            value: from_value(value, core_carrier, control)?,
            ordinal,
        };
        nodes.push(field, control)?;
    }
    normalize_fields(&mut nodes, control)?;
    Ok(Node::Object(nodes))
}

#[cfg(test)]
mod tests;

fn keys_equal(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> std::result::Result<bool, ValueRetentionError> {
    control.check()?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        control.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn field_index(
    fields: &[Field],
    key: &str,
    control: &ProductionControl<'_>,
) -> std::result::Result<Option<usize>, ValueRetentionError> {
    let mut start = 0;
    let mut end = fields.len();
    while start < end {
        let middle = start + (end - start) / 2;
        match compare_keys(&fields[middle].key, key, control)? {
            std::cmp::Ordering::Less => start = middle + 1,
            std::cmp::Ordering::Greater => end = middle,
            std::cmp::Ordering::Equal => return Ok(Some(middle)),
        }
    }
    control.check()?;
    Ok(None)
}

fn insert_field(
    fields: &mut Values<Field>,
    field: Field,
    control: &ProductionControl<'_>,
) -> Result<usize> {
    let mut start = 0;
    let mut end = fields.len();
    while start < end {
        let middle = start + (end - start) / 2;
        if compare_keys(&fields[middle].key, &field.key, control)?.is_lt() {
            start = middle + 1;
        } else {
            end = middle;
        }
    }
    fields.insert(start, field, control)?;
    Ok(start)
}

fn normalize_fields(
    fields: &mut Values<Field>,
    control: &ProductionControl<'_>,
) -> std::result::Result<(), ValueRetentionError> {
    uqa_core::ordering::sort_by_with_control(
        fields.as_mut_slice(),
        &mut || control.check(),
        |left, right, _| {
            Ok(compare_keys(&left.key, &right.key, control)?
                .then_with(|| right.ordinal.cmp(&left.ordinal)))
        },
    )?;
    let mut retained = 0;
    for index in 0..fields.len() {
        control.check()?;
        if retained == 0 || !keys_equal(&fields[retained - 1].key, &fields[index].key, control)? {
            fields.as_mut_slice().swap(retained, index);
            retained += 1;
        }
    }
    fields.truncate(retained);
    Ok(())
}

fn compare_keys(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> std::result::Result<std::cmp::Ordering, ValueRetentionError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        control.check()?;
        let order = left.cmp(right);
        if !order.is_eq() {
            return Ok(order);
        }
    }
    control.check()?;
    Ok(left.len().cmp(&right.len()))
}

fn inline(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn text_value(
    text: Produced<String>,
    jsonb: Option<bool>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (text, memory) = text.into_parts();
    Ok(control.finish(
        match jsonb {
            Some(true) => Value::JsonB(text),
            Some(false) => Value::Json(text),
            None => Value::Str(text),
        },
        memory,
    )?)
}

fn typed(node: &Node, jsonb: bool, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    text_value(writer::format(node, jsonb, control)?, Some(jsonb), control)
}

fn input(value: &Value, control: &ProductionControl<'_>) -> Result<Node> {
    parsed::parse(
        &super::super::conversion::value_to_string_with_control(value, control)?,
        control,
    )
}

pub(in crate::expr) use access::json_extract_operator_with_control;
pub(in crate::expr) use functions::evaluate;
#[cfg(test)]
pub(in crate::expr) use mutation::json_delete_with_control;
pub(in crate::expr) use mutation::{json_concat_with_control, json_delete_values_with_control};
pub(in crate::expr) use writer::quote_with_control;
