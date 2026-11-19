//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A value visitor observes sorted fields and numeric visitors after physical decoding.

use super::{read, JsonDocument, JsonValue, Value};
use crate::{
    json::{
        read_json_number_text, JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError,
        JsonReadWork, JsonScalarProtocol,
    },
    memory::{
        Budgeted, BudgetedMap, BudgetedMapIter, BudgetedVec, OwnedMap, ProductionControl,
        ProductionString,
    },
};

pub(in crate::json) mod numbers;

#[derive(Clone, Copy)]
enum Projection {
    Generic,
    Buffered,
}

type Result<T> = std::result::Result<T, JsonInputError>;
enum Frame<'a> {
    Value(usize),
    Array {
        remaining: std::slice::Iter<'a, usize>,
        values: BudgetedVec<usize>,
    },
    Object {
        remaining: Budgeted<Box<BudgetedMapIter<'a, String, usize>>>,
        values: OwnedMap<String, usize>,
        key: Option<Budgeted<String>>,
    },
}

pub(super) async fn value<W: JsonReadWork<Error = JsonReadError>>(
    source: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<JsonDocument> {
    project(source, control, work, protocol, Projection::Generic).await
}

pub(super) async fn buffered<W: JsonReadWork<Error = JsonReadError>>(
    source: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<JsonDocument> {
    project(source, control, work, protocol, Projection::Buffered).await
}

async fn project<W: JsonReadWork<Error = JsonReadError>>(
    source: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
    projection: Projection,
) -> Result<JsonDocument> {
    let memory = control.budget().expect("admitted generic projection");
    let mut document = JsonDocument {
        nodes: BudgetedMap::new(memory),
        next: 0,
        root: 0,
    };
    let mut frames = BudgetedVec::new(memory);
    frames.push(Frame::Value(source.index))?;
    while let Some(frame) = frames.pop() {
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        let completed = match frame {
            Frame::Value(index) => {
                let value = JsonValue {
                    document: source.document,
                    index,
                };
                enter(
                    &mut document,
                    value,
                    &mut frames,
                    control,
                    work,
                    protocol,
                    projection,
                )
                .await?
            }
            Frame::Array {
                mut remaining,
                values,
            } => {
                if let Some(&next) = remaining.next() {
                    frames.push(Frame::Array { remaining, values })?;
                    frames.push(Frame::Value(next))?;
                    None
                } else {
                    Some(document.insert(Value::Array(values))?)
                }
            }
            Frame::Object {
                mut remaining,
                values,
                key: None,
            } => {
                if let Some((name, &next)) = remaining.as_mut().next() {
                    let key = copy_text(name, control, work).await?;
                    frames.push(Frame::Object {
                        remaining,
                        values,
                        key: Some(key),
                    })?;
                    frames.push(Frame::Value(next))?;
                    None
                } else {
                    Some(document.insert(Value::Object(values))?)
                }
            }
            Frame::Object { key: Some(_), .. } => {
                unreachable!("projected child completes before another field")
            }
        };
        if let Some(index) = completed {
            match frames.last_mut() {
                Some(Frame::Array { values, .. }) => values.push(index)?,
                Some(Frame::Object { values, key, .. }) => {
                    let key = key.take().expect("selected projected field");
                    let replaced = values
                        .insert_admitted_key_from(
                            key,
                            index,
                            &mut read::Comparison { control, work },
                        )
                        .await?;
                    debug_assert!(replaced.is_none(), "projected names are unique");
                }
                Some(Frame::Value(_)) => unreachable!("child parent is a container"),
                None => {
                    document.root = index;
                    control.check().map_err(JsonReadError::from)?;
                    return Ok(document);
                }
            }
        }
    }
    unreachable!("complete generic root")
}

async fn enter<'a, W: JsonReadWork<Error = JsonReadError>>(
    document: &mut JsonDocument,
    source: JsonValue<'a>,
    frames: &mut BudgetedVec<Frame<'a>>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
    projection: Projection,
) -> Result<Option<usize>> {
    let memory = control.budget().expect("admitted generic projection");
    let value = source;
    Ok(match value.value() {
        Value::Null => Some(document.insert(Value::Null)?),
        Value::Boolean(value) => Some(document.insert(Value::Boolean(*value))?),
        Value::Number(text) => Some(match projection {
            Projection::Generic => document.insert(Value::Number(
                copy_text(numbers::project(text).as_str(), control, work).await?,
            ))?,
            Projection::Buffered => {
                numbers::buffered(document, text, control, work, protocol).await?
            }
        }),
        Value::String(text) => {
            Some(document.insert(Value::String(copy_text(text, control, work).await?))?)
        }
        Value::Array(values) => {
            frames.push(Frame::Array {
                remaining: values.iter(),
                values: BudgetedVec::new(memory),
            })?;
            None
        }
        Value::Object(fields) => {
            if let Some((name, &index)) = fields.iter().next().filter(|(name, _)| {
                matches!(projection, Projection::Generic)
                    && (name.as_str() == protocol.number || name.as_str() == protocol.raw)
            }) {
                let child = JsonValue {
                    document: source.document,
                    index,
                };
                let value = scalar(
                    document,
                    child,
                    name.as_str() == protocol.number,
                    control,
                    work,
                    protocol,
                )
                .await?;
                if fields.len() != 1 {
                    return Err(JsonDiagnostic::invalid_length(
                        fields.len(),
                        "fewer elements in map",
                        JsonPosition::unlocated(),
                    )
                    .into());
                }
                Some(value)
            } else {
                let allocation = memory.reserve(size_of::<BudgetedMapIter<'_, String, usize>>())?;
                frames.push(Frame::Object {
                    remaining: Budgeted::new(Box::new(fields.iter()), allocation),
                    values: OwnedMap::new(),
                    key: None,
                })?;
                None
            }
        }
    })
}

async fn scalar<W: JsonReadWork<Error = JsonReadError>>(
    output: &mut JsonDocument,
    value: JsonValue<'_>,
    number: bool,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<usize> {
    let Some(text) = value.as_str() else {
        return Err(JsonDiagnostic::projected_type_with_work(
            value,
            if number {
                "string containing a number"
            } else {
                "raw value"
            },
            control,
            work,
        )
        .await?
        .into());
    };
    if number {
        read_json_number_text(text, control, &mut crate::json::work::InputWork(work))
            .await
            .map_err(crate::json::value::scalar_error)?;
        output.insert(Value::Number(
            read::number_text(text.as_bytes(), control, work).await?,
        ))
    } else {
        read::text(output, text.as_bytes(), control, work, protocol)
            .await
            .map_err(crate::json::value::scalar_error)
    }
}

pub(super) async fn copy_text<W: JsonReadWork<Error = JsonReadError>>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Budgeted<String>> {
    let mut output = ProductionString::new(*control);
    let mut offset = 0;
    while offset < text.len() {
        work.checkpoint().await?;
        let mut end = (offset + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        output
            .push_str(&text[offset..end])
            .map_err(JsonReadError::from)?;
        offset = end;
    }
    Ok(output
        .finish()
        .map_err(JsonReadError::from)?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("admitted projected text")))
}
