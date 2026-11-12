//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic value validation retains only the unsigned scalar observation needed by the format owner.

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
enum Frame {
    Array,
    Object { first: bool },
    Scalar(Kind),
    ScalarValue(Option<u64>),
}

pub(super) async fn read<'a, W: JsonReadWork<Error = JsonReadError>>(
    mut token: Token<'a>,
    reader: &mut JsonReader<'a, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: Keys,
) -> StorageBackendResult<Option<u64>> {
    let mut frames = BudgetedVec::new(control.budget().expect("admitted marker input"));
    loop {
        control.check()?;
        let pending = match frames.last() {
            Some(Frame::Scalar(kind)) => Some(*kind),
            _ => None,
        };
        let completed = if let Some(kind) = pending {
            let JsonToken::String(encoded) = token else {
                return Err(invalid());
            };
            let text = decode_json_string_with_work(encoded, control, work)
                .await
                .map_err(json_error)?;
            let value = match kind {
                Kind::Number => number_text(&text, control, work).await?,
                Kind::Raw => raw(&text, *control, work, keys)?.await?,
            };
            Some(value)
        } else {
            match token {
                JsonToken::StartArray => {
                    frames.push(Frame::Array)?;
                    None
                }
                JsonToken::StartObject => {
                    frames.push(Frame::Object { first: true })?;
                    None
                }
                JsonToken::Key(encoded) => {
                    let Some(Frame::Object { first }) = frames.last_mut() else {
                        return Err(invalid());
                    };
                    if *first {
                        *first = false;
                        let name = decode_json_string_with_work(encoded, control, work)
                            .await
                            .map_err(json_error)?;
                        let kind = if name.as_str() == keys.number {
                            Some(Kind::Number)
                        } else if name.as_str() == keys.raw {
                            Some(Kind::Raw)
                        } else {
                            None
                        };
                        if let Some(kind) = kind {
                            *frames.last_mut().expect("object") = Frame::Scalar(kind);
                        }
                    }
                    None
                }
                JsonToken::EndArray => {
                    if !matches!(frames.pop(), Some(Frame::Array)) {
                        return Err(invalid());
                    }
                    Some(None)
                }
                JsonToken::EndObject => Some(match frames.pop() {
                    Some(Frame::Object { .. }) => None,
                    Some(Frame::ScalarValue(value)) => value,
                    _ => return Err(invalid()),
                }),
                JsonToken::Number(bytes) => Some(unsigned(bytes)),
                JsonToken::Null | JsonToken::Bool(_) | JsonToken::String(_) => Some(None),
            }
        };
        if let Some(value) = completed {
            if frames.is_empty() {
                return Ok(value);
            }
            if let Some(frame @ Frame::Scalar(_)) = frames.last_mut() {
                *frame = Frame::ScalarValue(value);
            }
        }
        token = next(reader, work).await?;
    }
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
    Budgeted<Pin<Box<dyn Future<Output = StorageBackendResult<Option<u64>>> + Send + 'a>>>;

fn raw<'a, W: JsonReadWork<Error = JsonReadError> + 'a>(
    text: &'a str,
    control: ProductionControl<'a>,
    work: &'a mut W,
    keys: Keys,
) -> StorageBackendResult<Raw<'a>> {
    let future = async move {
        let mut reader = JsonReader::with_control(text, &control).with_depth_limit(127);
        let first = next(&mut reader, work).await?;
        let value = read(first, &mut reader, &control, work, keys).await?;
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
