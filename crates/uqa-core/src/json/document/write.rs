//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{JsonValue, Value};
use crate::{
    json::{JsonInputError, JsonReadError, JsonReadWork},
    memory::{Budgeted, BudgetedMapIter, BudgetedVec, ProductionControl, ProductionString},
};

enum Frame<'a> {
    Value(usize),
    Array {
        items: std::slice::Iter<'a, usize>,
        first: bool,
    },
    Object {
        items: Budgeted<Box<BudgetedMapIter<'a, String, usize>>>,
        first: bool,
    },
}

pub(super) async fn value<W: JsonReadWork<Error = JsonReadError>>(
    value: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Budgeted<String>, JsonInputError> {
    let mut output = ProductionString::new(*control);
    let mut frames = BudgetedVec::new(control.budget().expect("admitted JSON output"));
    frames.push(Frame::Value(value.index))?;
    while let Some(frame) = frames.pop() {
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        match frame {
            Frame::Value(index) => {
                let value = &value
                    .document
                    .nodes
                    .get(&index)
                    .expect("JSON output node")
                    .value;
                match value {
                    Value::Null => output.push_str("null").map_err(JsonReadError::from)?,
                    Value::Boolean(value) => output
                        .push_str(if *value { "true" } else { "false" })
                        .map_err(JsonReadError::from)?,
                    Value::Number(value) => text(value, &mut output, work).await?,
                    Value::String(value) => string(value, &mut output, work).await?,
                    Value::Array(values) => {
                        output.push('[').map_err(JsonReadError::from)?;
                        frames.push(Frame::Array {
                            items: values.iter(),
                            first: true,
                        })?;
                    }
                    Value::Object(values) => {
                        output.push('{').map_err(JsonReadError::from)?;
                        let memory = control
                            .budget()
                            .expect("admitted JSON output")
                            .reserve(size_of::<BudgetedMapIter<'_, String, usize>>())?;
                        frames.push(Frame::Object {
                            items: Budgeted::new(Box::new(values.iter()), memory),
                            first: true,
                        })?;
                    }
                }
            }
            Frame::Array { mut items, first } => {
                if let Some(&index) = items.next() {
                    if !first {
                        output.push(',').map_err(JsonReadError::from)?;
                    }
                    frames.push(Frame::Array {
                        items,
                        first: false,
                    })?;
                    frames.push(Frame::Value(index))?;
                } else {
                    output.push(']').map_err(JsonReadError::from)?;
                }
            }
            Frame::Object { mut items, first } => {
                if let Some((name, &index)) = items.as_mut().next() {
                    if !first {
                        output.push(',').map_err(JsonReadError::from)?;
                    }
                    string(name, &mut output, work).await?;
                    output.push(':').map_err(JsonReadError::from)?;
                    frames.push(Frame::Object {
                        items,
                        first: false,
                    })?;
                    frames.push(Frame::Value(index))?;
                } else {
                    output.push('}').map_err(JsonReadError::from)?;
                }
            }
        }
    }
    Ok(output
        .finish()
        .map_err(JsonReadError::from)?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("admitted JSON output")))
}

async fn text<W: JsonReadWork<Error = JsonReadError>>(
    value: &str,
    output: &mut ProductionString<'_>,
    work: &mut W,
) -> Result<(), JsonInputError> {
    let mut offset = 0;
    while offset < value.len() {
        work.checkpoint().await?;
        let mut end = (offset + 4096).min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        output
            .push_str(&value[offset..end])
            .map_err(JsonReadError::from)?;
        offset = end;
    }
    Ok(())
}

async fn string<W: JsonReadWork<Error = JsonReadError>>(
    value: &str,
    output: &mut ProductionString<'_>,
    work: &mut W,
) -> Result<(), JsonInputError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push('"').map_err(JsonReadError::from)?;
    let mut offset = 0;
    while offset < value.len() {
        work.checkpoint().await?;
        let mut end = (offset + 4096).min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        for ch in value[offset..end].chars() {
            match ch {
                '"' => output.push_str("\\\"").map_err(JsonReadError::from)?,
                '\\' => output.push_str("\\\\").map_err(JsonReadError::from)?,
                '\x08' => output.push_str("\\b").map_err(JsonReadError::from)?,
                '\x0c' => output.push_str("\\f").map_err(JsonReadError::from)?,
                '\n' => output.push_str("\\n").map_err(JsonReadError::from)?,
                '\r' => output.push_str("\\r").map_err(JsonReadError::from)?,
                '\t' => output.push_str("\\t").map_err(JsonReadError::from)?,
                ch if ch < ' ' => {
                    let byte = u8::try_from(ch).expect("ASCII control character");
                    output.push_str("\\u00").map_err(JsonReadError::from)?;
                    output
                        .push(char::from(HEX[usize::from(byte >> 4)]))
                        .map_err(JsonReadError::from)?;
                    output
                        .push(char::from(HEX[usize::from(byte & 15)]))
                        .map_err(JsonReadError::from)?;
                }
                ch => output.push(ch).map_err(JsonReadError::from)?,
            }
        }
        offset = end;
    }
    output.push('"').map_err(JsonReadError::from)?;
    Ok(())
}
