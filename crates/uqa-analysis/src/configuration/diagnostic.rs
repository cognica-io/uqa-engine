//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Diagnostic JSON compares with original public configuration fields without a value tree.

use super::view::{Array, Form, View};
use crate::Analyzer;
use std::{future::Future, pin::Pin};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{
        decode_json_string_from, observe_json_text, JsonInputError, JsonObjectProjection,
        JsonReadError, JsonReader, JsonScalarProtocol, JsonToken,
    },
    memory::{Budgeted, ProductionControl},
};

type Result<T> = std::result::Result<T, JsonReadError>;

/// A completely validated generic JSON value borrowing its original diagnostic text. Configuration defaults and aliases are not applied to this source.
pub struct AnalyzerDiagnostic<'a> {
    text: &'a str,
    protocol: JsonScalarProtocol,
}

impl<'a> AnalyzerDiagnostic<'a> {
    /// Validate every original value, including overwritten fields and public scalar representations, before any native configuration comparison.
    pub async fn read_from(
        text: &'a str,
        input: &mut AsyncImageInput<'_>,
    ) -> std::result::Result<Self, JsonInputError> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        let protocol = {
            let _memory = input.memory.reserve(128)?;
            JsonScalarProtocol::from_values(
                &serde_json::Number::from(1u8),
                serde_json::value::RawValue::NULL,
            )
            .map_err(|_| JsonReadError::InvalidJson)?
        };
        observe_json_text(text.as_bytes(), &control, input, protocol, None).await?;
        Ok(Self { text, protocol })
    }

    /// Compare with the public serialized configuration shape, including omitted synonym fields, explicit defaults, exact array order and sorted last-value object membership.
    pub async fn matches_from(
        &self,
        configuration: &Analyzer,
        input: &mut AsyncImageInput<'_>,
    ) -> std::result::Result<bool, JsonInputError> {
        compare(
            View::Analyzer(configuration),
            self.text.as_bytes(),
            self.protocol,
            input,
        )
        .await
        .map_err(Into::into)
    }
}

struct Source<'a> {
    original: &'a [u8],
    decoded: Option<Budgeted<String>>,
}

impl Source<'_> {
    fn bytes(&self) -> &[u8] {
        self.decoded
            .as_ref()
            .map_or(self.original, |value| value.as_bytes())
    }
}

async fn source<'a>(
    bytes: &'a [u8],
    protocol: JsonScalarProtocol,
    input: &mut AsyncImageInput<'_>,
) -> Result<Source<'a>> {
    let mut source = Source {
        original: bytes,
        decoded: None,
    };
    while let Some((text, number)) = scalar_source(source.bytes(), protocol, input).await? {
        source.decoded = Some(text);
        if number {
            break;
        }
    }
    Ok(source)
}

async fn scalar_source(
    bytes: &[u8],
    protocol: JsonScalarProtocol,
    input: &mut AsyncImageInput<'_>,
) -> Result<Option<(Budgeted<String>, bool)>> {
    let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
    let mut reader = JsonReader::from_slice_with_control(bytes, &control);
    if next(&mut reader, input).await?.token != JsonToken::StartObject {
        return Ok(None);
    }
    let JsonToken::Key(key) = next(&mut reader, input).await?.token else {
        return Ok(None);
    };
    let key = decode_json_string_from(key, input).await?;
    let number = key.as_str() == protocol.number;
    if !number && key.as_str() != protocol.raw {
        return Ok(None);
    }
    let JsonToken::String(text) = next(&mut reader, input).await?.token else {
        return Err(JsonReadError::InvalidJson);
    };
    Ok(Some((decode_json_string_from(text, input).await?, number)))
}

async fn compare(
    expected: View<'_>,
    bytes: &[u8],
    protocol: JsonScalarProtocol,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool> {
    input.checkpoint().await?;
    let source = source(bytes, protocol, input).await?;
    let bytes = source.bytes();
    let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
    let mut reader = JsonReader::from_slice_with_control(bytes, &control);
    let token = next(&mut reader, input).await?.token;
    match expected.form(false) {
        Form::Object(values) => {
            if token != JsonToken::StartObject {
                return Ok(false);
            }
            let fields =
                JsonObjectProjection::read_from(bytes, input.memory, input.cancellation, input)
                    .await?;
            if fields.len() != values.len() {
                return Ok(false);
            }
            for (key, value) in values {
                input.checkpoint().await?;
                let Some(bytes) = fields.get(key) else {
                    return Ok(false);
                };
                if !child(value, bytes, protocol, input)?.await? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Form::Array(values) => array(values, bytes, protocol, input).await,
        Form::Text(text) => text_equal(text, token, input).await,
        Form::Path(path) => {
            text_equal(
                path.to_str().ok_or(JsonReadError::InvalidJson)?,
                token,
                input,
            )
            .await
        }
        Form::Scalar(value) => {
            let _memory = input.memory.reserve(256)?;
            match (
                serde_json::to_value(value).map_err(|_| JsonReadError::InvalidJson)?,
                token,
            ) {
                (serde_json::Value::Null, JsonToken::Null) => Ok(true),
                (serde_json::Value::Bool(expected), JsonToken::Bool(actual)) => {
                    Ok(expected == actual)
                }
                (serde_json::Value::String(expected), token) => {
                    text_equal(&expected, token, input).await
                }
                (serde_json::Value::Number(expected), JsonToken::Number(bytes)) => {
                    // Native scalar numbers are usize/i32 integers. Longer valid spellings cannot equal their public numeric representation.
                    if bytes.len() > 21 {
                        return Ok(false);
                    }
                    let actual: serde_json::Number =
                        serde_json::from_slice(bytes).map_err(|_| JsonReadError::InvalidJson)?;
                    Ok(actual == expected)
                }
                _ => Ok(false),
            }
        }
    }
}

type Comparison<'a> = Budgeted<Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>>;

fn child<'a, 'c: 'a>(
    expected: View<'a>,
    bytes: &'a [u8],
    protocol: JsonScalarProtocol,
    input: &'a mut AsyncImageInput<'c>,
) -> Result<Comparison<'a>> {
    let memory = input.memory;
    let future = async move { compare(expected, bytes, protocol, input).await };
    let reservation = memory.reserve(size_of_val(&future))?;
    Ok(Budgeted::new(Box::pin(future), reservation))
}

async fn array(
    mut expected: Array<'_>,
    bytes: &[u8],
    protocol: JsonScalarProtocol,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool> {
    let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
    let mut reader = JsonReader::from_slice_with_control(bytes, &control);
    if next(&mut reader, input).await?.token != JsonToken::StartArray {
        return Ok(false);
    }
    loop {
        let event = next(&mut reader, input).await?;
        if event.token == JsonToken::EndArray {
            return Ok(expected.next().is_none());
        }
        let Some(value) = expected.next() else {
            return Ok(false);
        };
        let start = event.range.start;
        let mut end = event.range.end;
        let mut depth = usize::from(matches!(
            event.token,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            let event = next(&mut reader, input).await?;
            match event.token {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
            end = event.range.end;
        }
        if !child(value, &bytes[start..end], protocol, input)?.await? {
            return Ok(false);
        }
    }
}

async fn text_equal(
    expected: &str,
    actual: JsonToken<'_, &[u8]>,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool> {
    let JsonToken::String(encoded) = actual else {
        return Ok(false);
    };
    let actual = decode_json_string_from(encoded, input).await?;
    crate::json::bytes_equal(expected.as_bytes(), actual.as_bytes(), input).await
}

async fn next<'a>(
    reader: &mut JsonReader<'a, '_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<uqa_core::json::JsonEvent<'a, &'a [u8]>> {
    reader
        .next_event_from(input)
        .await?
        .ok_or(JsonReadError::InvalidJson)
}
