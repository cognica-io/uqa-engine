//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic value validation retains unsigned scalars and an optional field observation of the root object.

use super::{
    decode_json_string_with_work, invalid, json_error, next, protocol::Keys, JsonReadError,
    JsonReadWork, JsonReader, JsonToken, ProductionControl, StorageBackendResult, Token,
};
use std::{future::Future, pin::Pin};
use uqa_core::memory::{Budgeted, BudgetedVec};

#[derive(Clone, Copy)]
enum Kind {
    Number,
    Raw,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FieldProjection {
    pub present: bool,
    pub foreign: bool,
    pub unsigned: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) enum Observation {
    Other,
    Unsigned(u64),
    Object(FieldProjection),
}
impl Observation {
    fn from_unsigned(value: Option<u64>) -> Self {
        value.map_or(Self::Other, Self::Unsigned)
    }
    fn unsigned(self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(value),
            _ => None,
        }
    }
    pub(super) fn field(self) -> FieldProjection {
        match self {
            Self::Object(value) => value,
            _ => FieldProjection::default(),
        }
    }
}

enum Frame {
    Array,
    Object {
        first: bool,
        selected: bool,
        projection: Option<FieldProjection>,
    },
    Scalar {
        kind: Kind,
        projected: bool,
    },
    ScalarValue(Observation),
}

pub(super) async fn read<'a, W: JsonReadWork<Error = JsonReadError>>(
    token: Token<'a>,
    reader: &mut JsonReader<'a, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: Keys,
) -> StorageBackendResult<Option<u64>> {
    Ok(observe(token, reader, control, work, keys, None)
        .await?
        .unsigned())
}

pub(super) async fn observe<'a, W: JsonReadWork<Error = JsonReadError>>(
    mut token: Token<'a>,
    reader: &mut JsonReader<'a, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: Keys,
    field: Option<&'static str>,
) -> StorageBackendResult<Observation> {
    let mut frames = BudgetedVec::new(control.budget().expect("admitted marker input"));
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
            let text = decode_json_string_with_work(encoded, control, work)
                .await
                .map_err(json_error)?;
            let value = match kind {
                Kind::Number => {
                    Observation::from_unsigned(number_text(&text, control, work).await?)
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
                        (frames.is_empty() && field.is_some()).then(FieldProjection::default);
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
                    Some(Observation::Other)
                }
                JsonToken::EndObject => Some(match frames.pop() {
                    Some(Frame::Object { projection, .. }) => {
                        projection.map_or(Observation::Other, Observation::Object)
                    }
                    Some(Frame::ScalarValue(value)) => value,
                    _ => return Err(invalid()),
                }),
                JsonToken::Number(bytes) => Some(Observation::from_unsigned(unsigned(bytes))),
                JsonToken::Null | JsonToken::Bool(_) | JsonToken::String(_) => {
                    Some(Observation::Other)
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
    keys: Keys,
    field: Option<&'static str>,
) -> StorageBackendResult<()> {
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
    let name = decode_json_string_with_work(encoded, control, work)
        .await
        .map_err(json_error)?;
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
) -> StorageBackendResult<Option<u64>> {
    let mut reader = JsonReader::with_control(text, control);
    let event = reader
        .next_event_with_work(work)
        .await
        .map_err(json_error)?
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
    Budgeted<Pin<Box<dyn Future<Output = StorageBackendResult<Observation>> + Send + 'a>>>;

fn raw<'a, W: JsonReadWork<Error = JsonReadError> + 'a>(
    text: &'a str,
    control: ProductionControl<'a>,
    work: &'a mut W,
    keys: Keys,
    field: Option<&'static str>,
) -> StorageBackendResult<Raw<'a>> {
    let future = async move {
        let mut reader = JsonReader::with_control(text, &control).with_depth_limit(127);
        let first = next(&mut reader, work).await?;
        let value = observe(first, &mut reader, &control, work, keys, field).await?;
        if reader
            .next_event_with_work(work)
            .await
            .map_err(json_error)?
            .is_some()
        {
            return Err(invalid());
        }
        Ok(value)
    };
    let memory = control
        .budget()
        .expect("admitted raw marker input")
        .reserve(size_of_val(&future))?;
    Ok(Budgeted::new(Box::pin(future), memory))
}
