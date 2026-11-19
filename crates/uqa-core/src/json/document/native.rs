//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native literals preserve generic visitor observations and transfer independently admitted payloads.

use super::{
    project::numbers::{observe, Observation},
    JsonValue, Value as Generic,
};
use crate::{
    json::{
        JsonDiagnostic, JsonInputError, JsonMessageFormat, JsonPosition, JsonReadError,
        JsonReadWork, JsonScalarProtocol,
    },
    memory::{
        BudgetedMapIter, MemoryReservation, Produced, ProductionControl, ProductionString,
        ProductionVec,
    },
    types::work::{JsonWorkError, Stack},
    Value, ValueRetentionError,
};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, JsonInputError>;

enum Frame<'a, 'c> {
    Array {
        remaining: std::slice::Iter<'a, usize>,
        values: ProductionVec<'c, Value>,
    },
    Object {
        remaining: ObjectFields<'a>,
        fields: Fields,
        key: Option<Produced<String>>,
    },
}

struct ObjectFields<'a> {
    values: Box<BudgetedMapIter<'a, String, usize>>,
    _memory: Option<MemoryReservation>,
}

impl<'a> ObjectFields<'a> {
    fn new(
        values: BudgetedMapIter<'a, String, usize>,
        control: ProductionControl<'_>,
    ) -> Result<Self> {
        let memory = control
            .reserve(size_of_val(&values))
            .map_err(JsonReadError::from)?;
        Ok(Self {
            values: Box::new(values),
            _memory: memory,
        })
    }
}

impl<'a> Iterator for ObjectFields<'a> {
    type Item = (&'a String, &'a usize);
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
}

struct Fields {
    values: BTreeMap<String, Value>,
    memory: Option<MemoryReservation>,
}
impl Fields {
    fn new(control: ProductionControl<'_>) -> Self {
        Self {
            values: BTreeMap::new(),
            memory: control.empty_reservation(),
        }
    }
    fn insert(
        &mut self,
        key: Produced<String>,
        value: Produced<Value>,
        control: ProductionControl<'_>,
    ) -> Result<()> {
        let entry = control
            .reserve(size_of::<(String, Value)>())
            .map_err(JsonReadError::from)?;
        let (key, key_memory) = key.into_parts();
        let (value, value_memory) = value.into_parts();
        let old = self.values.insert(key, value);
        debug_assert!(old.is_none(), "generic field names are unique");
        self.memory = control.combine(
            self.memory.take(),
            control.combine(entry, control.combine(key_memory, value_memory)),
        );
        Ok(())
    }
    fn finish(self, control: ProductionControl<'_>) -> Result<Produced<BTreeMap<String, Value>>> {
        Ok(control
            .finish(self.values, self.memory)
            .map_err(JsonReadError::from)?)
    }
}

pub(super) async fn value<W: JsonReadWork<Error = JsonReadError>>(
    source: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<Produced<Value>> {
    let mut frames = Stack::new(control.budget());
    let mut pending = Some(source.index);
    loop {
        checkpoint(control, work).await?;
        let completed = if let Some(index) = pending.take() {
            let source = JsonValue {
                document: source.document,
                index,
            };
            enter(source, &mut frames, *control, work).await?
        } else {
            match frames.pop().expect("unfinished native container") {
                Frame::Array {
                    mut remaining,
                    values,
                } => {
                    if let Some(&index) = remaining.next() {
                        pending = Some(index);
                        frames.push(Frame::Array { remaining, values })?;
                        None
                    } else {
                        let (values, memory) =
                            values.finish().map_err(JsonReadError::from)?.into_parts();
                        Some(
                            control
                                .finish(Value::List(values), memory)
                                .map_err(JsonReadError::from)?,
                        )
                    }
                }
                Frame::Object {
                    mut remaining,
                    fields,
                    key: None,
                } => {
                    if let Some((name, &index)) = remaining.next() {
                        let key = text(name, control, work).await?;
                        pending = Some(index);
                        frames.push(Frame::Object {
                            remaining,
                            fields,
                            key: Some(key),
                        })?;
                        None
                    } else {
                        Some(object(fields.finish(*control)?, *control, work, protocol).await?)
                    }
                }
                Frame::Object { key: Some(_), .. } => {
                    unreachable!("native field value completes before the next field")
                }
            }
        };
        if let Some(value) = completed {
            match frames.last_mut() {
                Some(Frame::Array { values, .. }) => {
                    values.push_produced(value).map_err(JsonReadError::from)?;
                }
                Some(Frame::Object { fields, key, .. }) => {
                    fields.insert(key.take().expect("selected native field"), value, *control)?;
                }
                None => {
                    control.check().map_err(JsonReadError::from)?;
                    return Ok(value);
                }
            }
        }
    }
}

async fn enter<'a, 'c, W: JsonReadWork<Error = JsonReadError>>(
    source: JsonValue<'a>,
    frames: &mut Stack<Frame<'a, 'c>>,
    control: ProductionControl<'c>,
    work: &mut W,
) -> Result<Option<Produced<Value>>> {
    let scalar = match source.value() {
        Generic::Null => Value::Null,
        Generic::Boolean(value) => Value::Bool(*value),
        Generic::Number(text) => {
            let value = match observe(text) {
                Observation::Unsigned(value) => {
                    i64::try_from(value).map_or(Value::Float(value as f64), Value::Int)
                }
                Observation::Signed(value) => Value::Int(value),
                Observation::Float(value) => Value::Float(value),
                Observation::WideSigned(value) => {
                    return Err(JsonDiagnostic::wide_signed(value, "a UQA value").into())
                }
                Observation::WideUnsigned(value) => {
                    return Err(JsonDiagnostic::wide_unsigned(value, "a UQA value").into())
                }
                Observation::Map(text) => return number(text, control, work).await.map(Some),
            };
            value
        }
        Generic::String(value) => {
            let (value, memory) = text(value, &control, work).await?.into_parts();
            return Ok(Some(
                control
                    .finish(Value::Str(value), memory)
                    .map_err(JsonReadError::from)?,
            ));
        }
        Generic::Array(values) => {
            frames.push(Frame::Array {
                remaining: values.iter(),
                values: ProductionVec::new(control),
            })?;
            return Ok(None);
        }
        Generic::Object(fields) => {
            frames.push(Frame::Object {
                remaining: ObjectFields::new(fields.iter(), control)?,
                fields: Fields::new(control),
                key: None,
            })?;
            return Ok(None);
        }
    };
    Ok(Some(
        control
            .finish(scalar, control.empty_reservation())
            .map_err(JsonReadError::from)?,
    ))
}

async fn object<W: JsonReadWork<Error = JsonReadError>>(
    fields: Produced<BTreeMap<String, Value>>,
    control: ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<Produced<Value>> {
    if fields.len() == 1 {
        if let Some(Value::Str(text)) = fields.get(protocol.number) {
            return number(text, control, work).await;
        }
    }
    match Value::from_projected_json_fields(fields, control, work).await {
        Ok(value) => Ok(value),
        Err(error) => Err(diagnostic(error, &control, work).await?),
    }
}

async fn number<W: JsonReadWork<Error = JsonReadError>>(
    text: &str,
    control: ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<Value>> {
    checkpoint(&control, work).await?;
    match Value::from_projected_json_number(text, control, work).await {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(JsonDiagnostic::message(
            "invalid arbitrary-precision JSON number",
            JsonPosition::unlocated(),
        )
        .into()),
        Err(error) => Err(diagnostic(error, &control, work).await?),
    }
}

async fn diagnostic<W: JsonReadWork<Error = JsonReadError>>(
    error: JsonWorkError,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<JsonInputError> {
    match error {
        JsonWorkError::Work(error) => Ok(error.into()),
        JsonWorkError::Native(ValueRetentionError::Malformed { kind, reason }) => {
            let mut message = ProductionString::new(*control);
            for part in ["malformed ", kind, " value: ", reason.as_str()] {
                let text = text(part, control, work).await?;
                message.push_str(&text).map_err(JsonReadError::from)?;
            }
            Ok(JsonDiagnostic::retained_message(
                &const { JsonMessageFormat::new("", "", "malformed native value") },
                message.finish().map_err(JsonReadError::from)?,
                JsonPosition::unlocated(),
            )
            .into())
        }
        JsonWorkError::Native(error) => Ok(JsonReadError::from(error).into()),
    }
}

async fn checkpoint<W: JsonReadWork<Error = JsonReadError>>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<()> {
    control.check().map_err(JsonReadError::from)?;
    work.checkpoint().await?;
    control.check().map_err(JsonReadError::from)?;
    Ok(())
}

async fn text<W: JsonReadWork<Error = JsonReadError>>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    output.reserve(text.len()).map_err(JsonReadError::from)?;
    let mut begin = 0;
    while begin < text.len() {
        checkpoint(control, work).await?;
        let mut end = (begin + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        output
            .push_str(&text[begin..end])
            .map_err(JsonReadError::from)?;
        begin = end;
    }
    Ok(output.finish().map_err(JsonReadError::from)?)
}
