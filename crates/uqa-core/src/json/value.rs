//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic value validation retains unsigned scalars and an optional field observation of the root object.

use super::{decode_json_string_with_work, JsonReadError, JsonReadWork, JsonReader, JsonToken};
use crate::memory::{Budgeted, BudgetedVec, ProductionControl};
use std::{future::Future, pin::Pin};

mod protocol;
pub use protocol::JsonScalarProtocol;

type JsonResult<T> = Result<T, JsonReadError>;
type Token<'a> = JsonToken<'a, &'a [u8]>;

#[derive(Clone, Copy)]
enum Kind {
    Number,
    Raw,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsonFieldProjection {
    pub present: bool,
    pub foreign: bool,
    pub unsigned: Option<u64>,
}

#[derive(Clone, Copy)]
pub enum JsonValueObservation {
    Other,
    Unsigned(u64),
    Object(JsonFieldProjection),
}
impl JsonValueObservation {
    fn from_unsigned(value: Option<u64>) -> Self {
        value.map_or(Self::Other, Self::Unsigned)
    }
    fn unsigned(self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(value),
            _ => None,
        }
    }
    pub fn field(self) -> JsonFieldProjection {
        match self {
            Self::Object(value) => value,
            _ => JsonFieldProjection::default(),
        }
    }
}

enum Frame {
    Array,
    Object {
        first: bool,
        selected: bool,
        projection: Option<JsonFieldProjection>,
    },
    Scalar {
        kind: Kind,
        projected: bool,
    },
    ScalarValue(JsonValueObservation),
}

/// Validate one generic value, including every overwritten child, and return its unsigned scalar observation. The reader's enclosing owner checks trailing input; the control must carry allocation admission.
pub async fn read_json_unsigned<'a, W: JsonReadWork<Error = JsonReadError>>(
    token: Token<'a>,
    reader: &mut JsonReader<'a, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: JsonScalarProtocol,
) -> JsonResult<Option<u64>> {
    Ok(observe_json_value(token, reader, control, work, keys, None)
        .await?
        .unsigned())
}

/// Validate one generic value without constructing its tree. A root field observation retains presence, other-field membership and the selected field's last unsigned value. Public number/raw scalar representations retain their original first-key interpretation.
pub async fn observe_json_value<'a, W: JsonReadWork<Error = JsonReadError>>(
    mut token: Token<'a>,
    reader: &mut JsonReader<'a, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: JsonScalarProtocol,
    field: Option<&'static str>,
) -> JsonResult<JsonValueObservation> {
    let mut frames = BudgetedVec::new(control.budget().expect("admitted generic JSON input"));
    loop {
        control.check()?;
        let pending = match frames.last() {
            Some(Frame::Scalar { kind, projected }) => Some((*kind, *projected)),
            _ => None,
        };
        let completed = if let Some((kind, projected)) = pending {
            let JsonToken::String(encoded) = token else {
                return Err(invalid());
            };
            let text = decode_json_string_with_work(encoded, control, work).await?;
            let value = match kind {
                Kind::Number => {
                    JsonValueObservation::from_unsigned(number_text(&text, control, work).await?)
                }
                Kind::Raw => raw(&text, *control, work, keys, field.filter(|_| projected))?.await?,
            };
            Some(value)
        } else {
            match token {
                JsonToken::StartArray => {
                    frames.push(Frame::Array)?;
                    None
                }
                JsonToken::StartObject => {
                    let projection =
                        (frames.is_empty() && field.is_some()).then(JsonFieldProjection::default);
                    frames.push(Frame::Object {
                        first: true,
                        selected: false,
                        projection,
                    })?;
                    None
                }
                JsonToken::Key(encoded) => {
                    key(
                        encoded,
                        frames.last_mut().ok_or_else(invalid)?,
                        control,
                        work,
                        keys,
                        field,
                    )
                    .await?;
                    None
                }
                JsonToken::EndArray => {
                    if !matches!(frames.pop(), Some(Frame::Array)) {
                        return Err(invalid());
                    }
                    Some(JsonValueObservation::Other)
                }
                JsonToken::EndObject => Some(match frames.pop() {
                    Some(Frame::Object { projection, .. }) => {
                        projection.map_or(JsonValueObservation::Other, JsonValueObservation::Object)
                    }
                    Some(Frame::ScalarValue(value)) => value,
                    _ => return Err(invalid()),
                }),
                JsonToken::Number(bytes) => {
                    Some(JsonValueObservation::from_unsigned(unsigned(bytes)))
                }
                JsonToken::Null | JsonToken::Bool(_) | JsonToken::String(_) => {
                    Some(JsonValueObservation::Other)
                }
            }
        };
        if let Some(value) = completed {
            if frames.is_empty() {
                return Ok(value);
            }
            match frames.last_mut() {
                Some(frame @ Frame::Scalar { .. }) => *frame = Frame::ScalarValue(value),
                Some(Frame::Object {
                    selected: true,
                    projection: Some(projection),
                    ..
                }) => projection.unsigned = value.unsigned(),
                _ => (),
            }
        }
        token = next(reader, work).await?;
    }
}

async fn key<W: JsonReadWork<Error = JsonReadError>>(
    encoded: &[u8],
    frame: &mut Frame,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: JsonScalarProtocol,
    field: Option<&'static str>,
) -> JsonResult<()> {
    let Frame::Object {
        first,
        selected,
        projection,
    } = frame
    else {
        return Err(invalid());
    };
    if !*first && projection.is_none() {
        return Ok(());
    }
    let name = decode_json_string_with_work(encoded, control, work).await?;
    let kind = if *first && name.as_str() == keys.number {
        Some(Kind::Number)
    } else if *first && name.as_str() == keys.raw {
        Some(Kind::Raw)
    } else {
        None
    };
    *first = false;
    if let Some(kind) = kind {
        *frame = Frame::Scalar {
            kind,
            projected: projection.is_some(),
        };
    } else if let Some(projection) = projection {
        *selected = field == Some(name.as_str());
        projection.present |= *selected;
        projection.foreign |= !*selected;
    }
    Ok(())
}

fn unsigned(bytes: &[u8]) -> Option<u64> {
    if bytes == b"-0" {
        return Some(0);
    }
    if bytes.len() > 20 {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

async fn number_text<W: JsonReadWork<Error = JsonReadError>>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> JsonResult<Option<u64>> {
    let mut reader = JsonReader::with_control(text, control);
    let event = reader
        .next_event_with_work(work)
        .await?
        .ok_or_else(invalid)?;
    let JsonToken::Number(bytes) = event.token else {
        return Err(invalid());
    };
    if event.range.start != 0 || event.range.end != text.len() {
        return Err(invalid());
    }
    Ok(unsigned(bytes))
}

type Raw<'a> =
    Budgeted<Pin<Box<dyn Future<Output = JsonResult<JsonValueObservation>> + Send + 'a>>>;

fn raw<'a, W: JsonReadWork<Error = JsonReadError> + 'a>(
    text: &'a str,
    control: ProductionControl<'a>,
    work: &'a mut W,
    keys: JsonScalarProtocol,
    field: Option<&'static str>,
) -> JsonResult<Raw<'a>> {
    let future = async move {
        let mut reader = JsonReader::with_control(text, &control).with_depth_limit(127);
        let first = next(&mut reader, work).await?;
        let value = observe_json_value(first, &mut reader, &control, work, keys, field).await?;
        if reader.next_event_with_work(work).await?.is_some() {
            return Err(invalid());
        }
        Ok(value)
    };
    let memory = control
        .budget()
        .expect("admitted raw JSON input")
        .reserve(size_of_val(&future))?;
    Ok(Budgeted::new(Box::pin(future), memory))
}

async fn next<'a, W: JsonReadWork<Error = JsonReadError>>(
    reader: &mut JsonReader<'a, '_>,
    work: &mut W,
) -> JsonResult<Token<'a>> {
    reader
        .next_event_with_work(work)
        .await?
        .map(|event| event.token)
        .ok_or_else(invalid)
}

fn invalid() -> JsonReadError {
    JsonReadError::InvalidJson
}

/// Validate a complete generic JSON text with the ordinary container-depth limit and no ignored suffix. The supplied control must retain an allocation allowance through all raw-scalar expansions.
pub async fn observe_json_text<W: JsonReadWork<Error = JsonReadError>>(
    text: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
    field: Option<&'static str>,
) -> JsonResult<JsonValueObservation> {
    let mut reader = JsonReader::from_slice_with_control(text, control).with_depth_limit(127);
    let first = next(&mut reader, work).await?;
    let observation =
        observe_json_value(first, &mut reader, control, work, protocol, field).await?;
    if reader.next_event_with_work(work).await?.is_some() {
        return Err(invalid());
    }
    control.check()?;
    Ok(observation)
}

#[cfg(test)]
mod tests;
