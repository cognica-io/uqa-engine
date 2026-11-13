//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generic value validation retains unsigned scalars and an optional field observation of the root object.

use super::{
    decode_json_string_with_work, JsonDiagnostic, JsonInputError, JsonReadError, JsonReadWork,
    JsonReader, JsonSyntaxError, JsonSyntaxKind, JsonToken,
};
use crate::memory::{Budgeted, BudgetedVec, ProductionControl};
use std::{future::Future, pin::Pin};

mod protocol;
pub use protocol::JsonScalarProtocol;

type JsonResult<T> = Result<T, JsonInputError>;
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
}

impl Frame {
    fn enter(self, reader: &mut JsonReader<'_, '_>) -> JsonResult<Self> {
        generic_container_depth(reader)?;
        Ok(self)
    }
}

pub(super) fn generic_container_depth(reader: &mut JsonReader<'_, '_>) -> JsonResult<()> {
    if reader.stack.frames().len() > 127 {
        return Err(JsonDiagnostic::from(JsonSyntaxError {
            kind: JsonSyntaxKind::DepthLimit,
            position: reader.position(),
        })
        .into());
    }
    Ok(())
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
        control.check().map_err(JsonReadError::from)?;
        let pending = match frames.last() {
            Some(Frame::Scalar { kind, projected }) => Some((*kind, *projected)),
            _ => None,
        };
        let completed = if let Some((kind, projected)) = pending {
            let value = scalar(
                kind,
                token,
                reader,
                control,
                work,
                keys,
                field.filter(|_| projected),
            )
            .await?;
            frames.pop();
            Some(value)
        } else {
            match token {
                JsonToken::StartArray => {
                    frames.push(Frame::Array.enter(reader)?)?;
                    None
                }
                JsonToken::StartObject => {
                    let projection =
                        (frames.is_empty() && field.is_some()).then(JsonFieldProjection::default);
                    frames.push(
                        Frame::Object {
                            first: true,
                            selected: false,
                            projection,
                        }
                        .enter(reader)?,
                    )?;
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
            if let Some(Frame::Object {
                selected: true,
                projection: Some(projection),
                ..
            }) = frames.last_mut()
            {
                projection.unsigned = value.unsigned();
            }
        }
        token = next(reader, work).await?;
    }
}

async fn scalar<'a, W: JsonReadWork<Error = JsonReadError>>(
    kind: Kind,
    token: Token<'a>,
    reader: &mut JsonReader<'a, '_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    keys: JsonScalarProtocol,
    field: Option<&'static str>,
) -> JsonResult<JsonValueObservation> {
    let JsonToken::String(encoded) = token else {
        let expected = match kind {
            Kind::Number => "string containing a number",
            Kind::Raw => "raw value",
        };
        return Err(JsonDiagnostic::invalid_type_with_work(
            token,
            reader.position(),
            expected,
            control,
            work,
        )
        .await?
        .into());
    };
    let text = decode_json_string_with_work(encoded, control, work).await?;
    let value = match kind {
        Kind::Number => JsonValueObservation::from_unsigned(
            number_text(&text, control, work)
                .await
                .map_err(scalar_error)?,
        ),
        Kind::Raw => raw(&text, *control, work, keys, field)?
            .await
            .map_err(scalar_error)?,
    };
    reader
        .finish_scalar_object(work)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error))?;
    Ok(value)
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
    read_json_number_text(text, control, &mut super::work::InputWork(work)).await
}

/// Validate the text supplied by a public numeric scalar representation. Original scalar suffix precedence and inner positions survive through the caller's work/error channel.
pub async fn read_json_number_text<W: JsonReadWork>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<u64>, W::Error>
where
    W::Error: From<JsonDiagnostic>,
{
    let mut reader = JsonReader::with_control(text, control);
    let result = match reader.peek() {
        Some(b'-' | b'0'..=b'9') => reader.number(work).await,
        Some(_) => Err(reader.reject(JsonSyntaxKind::InvalidNumber, true).into()),
        None => Err(reader.reject(JsonSyntaxKind::EndOfValue, false).into()),
    };
    if result.is_err() && reader.syntax_error().is_none() {
        return result.map(unsigned);
    }
    // The scalar number owner checks its immediate suffix even after a number-grammar failure. Resource failures have already terminated the operation.
    if reader.peek().is_some() {
        reader.reject(JsonSyntaxKind::InvalidNumber, true);
        return Err(
            JsonDiagnostic::from(reader.syntax_error().expect("rejected numeric suffix")).into(),
        );
    }
    result.map(unsigned).map_err(|error| {
        reader
            .syntax_error()
            .map_or(error, |error| JsonDiagnostic::from(error).into())
    })
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
        let mut reader = JsonReader::with_control(text, &control);
        let first = next(&mut reader, work).await?;
        let value = observe_json_value(first, &mut reader, &control, work, keys, field).await?;
        if reader
            .next_event_with_work(work)
            .await
            .map_err(|error| JsonInputError::from_reader(&reader, error))?
            .is_some()
        {
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
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error))?
        .map(|event| event.token)
        .ok_or_else(invalid)
}

fn invalid() -> JsonInputError {
    JsonInputError::Invalid
}

pub(super) fn scalar_error(error: JsonInputError) -> JsonInputError {
    match error {
        JsonInputError::Diagnostic(error) => error.into_data().into(),
        error => error,
    }
}

impl JsonReader<'_, '_> {
    /// A scalar visitor has consumed its only value. Retire the object at this boundary without requesting another key or inspecting a later value.
    pub async fn finish_scalar_object<W: JsonReadWork>(
        &mut self,
        work: &mut W,
    ) -> Result<(), W::Error> {
        self.skip_whitespace(work).await?;
        match self.peek() {
            Some(b'}') => {
                self.close(JsonToken::EndObject)?;
                Ok(())
            }
            Some(b',') => Err(self.reject(JsonSyntaxKind::TrailingComma, true).into()),
            None => Err(self.reject(JsonSyntaxKind::EndOfObject, false).into()),
            _ => Err(self.reject(JsonSyntaxKind::TrailingCharacters, true).into()),
        }
    }
}

/// Validate a complete generic JSON text with the ordinary container-depth limit and no ignored suffix. The supplied control must retain an allocation allowance through all raw-scalar expansions.
pub async fn observe_json_text<W: JsonReadWork<Error = JsonReadError>>(
    text: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
    field: Option<&'static str>,
) -> JsonResult<JsonValueObservation> {
    let mut reader = JsonReader::from_slice_with_control(text, control);
    let first = next(&mut reader, work).await?;
    let observation =
        observe_json_value(first, &mut reader, control, work, protocol, field).await?;
    if reader
        .next_event_with_work(work)
        .await
        .map_err(|error| JsonInputError::from_reader(&reader, error))?
        .is_some()
    {
        return Err(invalid());
    }
    control.check().map_err(JsonReadError::from)?;
    Ok(observation)
}

#[cfg(test)]
mod tests;
