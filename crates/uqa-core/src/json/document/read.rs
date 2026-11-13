//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{JsonDocument, Value};
use crate::{
    json::{
        decode_json_string_with_work, read_json_number_text, JsonDiagnostic, JsonInputError,
        JsonReadError, JsonReadWork, JsonReader, JsonScalarProtocol, JsonToken,
    },
    memory::{Budgeted, BudgetedVec, MapKeyInput, OwnedMap, ProductionControl, ProductionString},
};
use std::{cmp::Ordering, future::Future, pin::Pin};

type Result<T> = std::result::Result<T, JsonInputError>;
type Token<'a> = JsonToken<'a, &'a [u8]>;

enum Frame {
    Array(BudgetedVec<usize>),
    Object {
        fields: OwnedMap<String, usize>,
        key: Option<Budgeted<String>>,
    },
    Scalar {
        number: bool,
    },
}

pub(super) async fn text<W: JsonReadWork<Error = JsonReadError>>(
    document: &mut JsonDocument,
    bytes: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<usize> {
    let mut reader = JsonReader::from_slice_with_control(bytes, control);
    let root = value(document, &mut reader, control, work, protocol).await?;
    if reader
        .next_event_with_work(work)
        .await
        .map_err(|error| JsonInputError::from_reader(&reader, error))?
        .is_some()
    {
        return Err(JsonInputError::Invalid);
    }
    Ok(root)
}

async fn value<W: JsonReadWork<Error = JsonReadError>>(
    document: &mut JsonDocument,
    reader: &mut JsonReader<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<usize> {
    let memory = control.budget().expect("admitted JSON document");
    let mut frames = BudgetedVec::new(memory);
    loop {
        control.check().map_err(JsonReadError::from)?;
        let token = next(reader, work).await?;
        let completed = if let Some(Frame::Scalar { number }) = frames.last() {
            let index = scalar(document, reader, token, *number, control, work, protocol).await?;
            frames.pop();
            Some(index)
        } else {
            match token {
                JsonToken::StartArray => {
                    crate::json::value::generic_container_depth(reader)?;
                    frames.push(Frame::Array(BudgetedVec::new(memory)))?;
                    None
                }
                JsonToken::StartObject => {
                    crate::json::value::generic_container_depth(reader)?;
                    frames.push(Frame::Object {
                        fields: OwnedMap::new(),
                        key: None,
                    })?;
                    None
                }
                JsonToken::Key(encoded) => {
                    let name = decode_json_string_with_work(encoded, control, work)
                        .await?
                        .into_budgeted()
                        .unwrap_or_else(|_| unreachable!("admitted JSON key"));
                    let frame = frames.last_mut().ok_or(JsonInputError::Invalid)?;
                    let Frame::Object { fields, key } = frame else {
                        return Err(JsonInputError::Invalid);
                    };
                    if fields.is_empty()
                        && (name.as_str() == protocol.number || name.as_str() == protocol.raw)
                    {
                        *frame = Frame::Scalar {
                            number: name.as_str() == protocol.number,
                        };
                    } else {
                        *key = Some(name);
                    }
                    None
                }
                JsonToken::EndArray => {
                    let Some(Frame::Array(items)) = frames.pop() else {
                        return Err(JsonInputError::Invalid);
                    };
                    Some(document.insert(Value::Array(items))?)
                }
                JsonToken::EndObject => {
                    let Some(Frame::Object { fields, key: None }) = frames.pop() else {
                        return Err(JsonInputError::Invalid);
                    };
                    Some(document.insert(Value::Object(fields))?)
                }
                JsonToken::Null => Some(document.insert(Value::Null)?),
                JsonToken::Bool(value) => Some(document.insert(Value::Boolean(value))?),
                JsonToken::String(encoded) => Some(
                    document.insert(Value::String(
                        decode_json_string_with_work(encoded, control, work)
                            .await?
                            .into_budgeted()
                            .unwrap_or_else(|_| unreachable!("admitted JSON string")),
                    ))?,
                ),
                JsonToken::Number(bytes) => {
                    Some(document.insert(Value::Number(number_text(bytes, control, work).await?))?)
                }
            }
        };
        if let Some(index) = completed {
            match frames.last_mut() {
                Some(Frame::Array(items)) => items.push(index)?,
                Some(Frame::Object { fields, key }) => {
                    let key = key.take().ok_or(JsonInputError::Invalid)?;
                    let previous = fields
                        .insert_admitted_key_from(key, index, &mut Comparison { control, work })
                        .await?;
                    if let Some(previous) = previous {
                        document.retire(previous, control, work).await?;
                    }
                }
                Some(Frame::Scalar { .. }) => return Err(JsonInputError::Invalid),
                None => return Ok(index),
            }
        }
    }
}

async fn scalar<W: JsonReadWork<Error = JsonReadError>>(
    document: &mut JsonDocument,
    reader: &mut JsonReader<'_, '_>,
    token: Token<'_>,
    number: bool,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> Result<usize> {
    let JsonToken::String(encoded) = token else {
        return Err(JsonDiagnostic::invalid_type_with_work(
            token,
            reader.position(),
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
    let text = decode_json_string_with_work(encoded, control, work).await?;
    let index = if number {
        read_json_number_text(&text, control, &mut crate::json::work::InputWork(work))
            .await
            .map_err(crate::json::value::scalar_error)?;
        document.insert(Value::Number(
            number_text(text.as_bytes(), control, work).await?,
        ))?
    } else {
        raw(document, &text, *control, work, protocol)?
            .await
            .map_err(crate::json::value::scalar_error)?
    };
    reader
        .finish_scalar_object(work)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error))?;
    Ok(index)
}

async fn next<'a, W: JsonReadWork<Error = JsonReadError>>(
    reader: &mut JsonReader<'a, '_>,
    work: &mut W,
) -> Result<Token<'a>> {
    reader
        .next_event_with_work(work)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error))?
        .map(|event| event.token)
        .ok_or(JsonInputError::Invalid)
}

async fn number_text<W: JsonReadWork<Error = JsonReadError>>(
    text: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Budgeted<String>> {
    let mut output = ProductionString::new(*control);
    if text == b"-0" {
        output.push_str("0").map_err(JsonReadError::from)?;
    } else {
        for (chunk_index, chunk) in text.chunks(4096).enumerate() {
            work.checkpoint().await?;
            for (index, &byte) in chunk.iter().enumerate() {
                if matches!(byte, b'E' | b'e') {
                    output.push('e').map_err(JsonReadError::from)?;
                    if !matches!(text.get(chunk_index * 4096 + index + 1), Some(b'+' | b'-')) {
                        output.push('+').map_err(JsonReadError::from)?;
                    }
                } else {
                    output.push(char::from(byte)).map_err(JsonReadError::from)?;
                }
            }
        }
    }
    Ok(output
        .finish()
        .map_err(JsonReadError::from)?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("admitted JSON number")))
}

type Raw<'a> = Budgeted<Pin<Box<dyn Future<Output = Result<usize>> + Send + 'a>>>;
fn raw<'a, W: JsonReadWork<Error = JsonReadError> + 'a>(
    document: &'a mut JsonDocument,
    text: &'a str,
    control: ProductionControl<'a>,
    work: &'a mut W,
    protocol: JsonScalarProtocol,
) -> Result<Raw<'a>> {
    let future =
        async move { self::text(document, text.as_bytes(), &control, work, protocol).await };
    let memory = control
        .budget()
        .expect("admitted JSON raw value")
        .reserve(size_of_val(&future))?;
    Ok(Budgeted::new(Box::pin(future), memory))
}

struct Comparison<'a, 'c, W> {
    control: &'a ProductionControl<'c>,
    work: &'a mut W,
}
impl<W: JsonReadWork<Error = JsonReadError>> MapKeyInput<String> for Comparison<'_, '_, W> {
    type Error = JsonInputError;
    async fn checkpoint(&mut self) -> Result<()> {
        self.control.check().map_err(JsonReadError::from)?;
        self.work.checkpoint().await?;
        Ok(())
    }
    async fn compare(&mut self, query: &String, stored: &String) -> Result<Ordering> {
        for (left, right) in query
            .as_bytes()
            .chunks(4096)
            .zip(stored.as_bytes().chunks(4096))
        {
            self.checkpoint().await?;
            let order = left.cmp(right);
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        Ok(query.len().cmp(&stored.len()))
    }
}
