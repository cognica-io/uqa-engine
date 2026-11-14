//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed declarations consume already projected values through their original work owner.

use super::{InputError, Native, Parts};
use std::{future::Future, pin::Pin};
use uqa_core::{
    json::{
        JsonDiagnostic, JsonDocument, JsonInputError, JsonPosition, JsonReadError, JsonReadWork,
        JsonScalarProtocol, JsonValue, JsonValueKind,
    },
    memory::{MemoryReservation, Produced, ProductionControl, ProductionString},
};

mod work;
pub(in crate::ast::query_input) use work::run_with;
pub(super) use work::Work;
mod fields;
mod flatten;
pub(super) use fields::Fields;
pub(super) use flatten::FlatFields;
mod assignments;
mod attributes;
mod bindings;
mod collections;
pub(super) mod compound;
mod expressions;
mod identities;
mod roles;
mod scalars;
pub(super) mod special;
mod types;

pub(super) type Result<T> = std::result::Result<T, JsonInputError>;
pub(super) struct Read<'a, T> {
    future: Pin<Box<dyn Future<Output = Result<Native<T>>> + Send + 'a>>,
    // The future's owned allocation retires before its admission.
    _memory: Option<MemoryReservation>,
}

impl<T> Future for Read<'_, T> {
    type Output = Result<Native<T>>;
    fn poll(
        self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(context)
    }
}

pub(super) trait Projected: Sized + Send + 'static {
    const BUFFERED: bool = false;
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>>;
}

#[derive(Clone, Copy)]
pub(super) struct Input<'a> {
    pub value: JsonValue<'a>,
    pub control: ProductionControl<'a>,
    pub protocol: JsonScalarProtocol,
    pub buffered: bool,
}

impl<'a> Input<'a> {
    pub fn child(self, value: JsonValue<'a>) -> Self {
        Self { value, ..self }
    }

    pub async fn checkpoint(
        self,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<()> {
        self.control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        self.control.check().map_err(JsonReadError::from)?;
        Ok(())
    }

    pub async fn read<T: Projected>(self, work: &mut Work<'a>) -> Result<Native<T>> {
        work::receive(work.submit::<T>(self)?).await
    }

    pub fn run<'w, T: Projected, W: JsonReadWork<Error = JsonReadError> + 'w>(
        self,
        work: &'w mut W,
    ) -> Result<Read<'w, T>>
    where
        'a: 'w,
    {
        work::run(self, work)
    }

    pub async fn type_error(
        self,
        expected: &'static str,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<JsonInputError> {
        let diagnostic = if self.buffered {
            JsonDiagnostic::buffered_type_with_work(self.value, expected, &self.control, work)
                .await?
        } else {
            JsonDiagnostic::projected_type_with_work(self.value, expected, &self.control, work)
                .await?
        };
        Ok(diagnostic.into())
    }

    pub async fn buffer(
        self,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<Option<JsonDocument>> {
        if self.buffered {
            return Ok(None);
        }
        Ok(Some(
            self.value
                .buffer_with_work(&self.control, work, self.protocol)
                .await?,
        ))
    }

    pub fn buffered_value<'b>(self, prepared: Option<&'b JsonDocument>) -> Input<'b>
    where
        'a: 'b,
    {
        Input {
            value: prepared.map_or(self.value, JsonDocument::root),
            buffered: true,
            control: self.control,
            protocol: self.protocol,
        }
    }

    pub fn scalar<T>(self, value: T) -> Result<Native<T>> {
        native(
            self.control
                .finish(value, self.control.empty_reservation())
                .map_err(JsonReadError::from)?,
            self.control,
        )
    }

    pub async fn text(
        self,
        text: &str,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<Produced<String>> {
        let mut output = ProductionString::new(self.control);
        output.reserve(text.len()).map_err(JsonReadError::from)?;
        let mut begin = 0;
        while begin < text.len() {
            self.checkpoint(work).await?;
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

    pub async fn tag(self, work: &mut impl JsonReadWork<Error = JsonReadError>) -> Result<Tag<'a>> {
        self.checkpoint(work).await?;
        if let Some(name) = self.value.as_str() {
            return Ok(Tag { name, value: None });
        }
        let Some(fields) = self.value.fields() else {
            return Err(self.type_error("string or map", work).await?);
        };
        let mut fields = iterator(self.control, fields)?;
        let Some((name, value)) = fields.next() else {
            return Err(enum_map());
        };
        if fields.next().is_some() {
            return Err(enum_map());
        }
        Ok(Tag {
            name,
            value: Some(value),
        })
    }

    pub async fn unknown_variant(
        self,
        tag: &str,
        expected: &'static [&'static str],
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<JsonInputError> {
        Ok(JsonDiagnostic::unknown_variant(
            self.text(tag, work).await?,
            expected,
            JsonPosition::unlocated(),
        )
        .into())
    }
}

pub(super) struct Tag<'a> {
    pub name: &'a str,
    pub value: Option<JsonValue<'a>>,
}
impl<'a> Tag<'a> {
    pub async fn structure(
        self,
        input: Input<'a>,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<Input<'a>> {
        let child = input.child(self.payload("struct variant")?);
        if child.value.kind() != JsonValueKind::Object
            && !(input.buffered && child.value.kind() == JsonValueKind::Array)
        {
            return Err(child.type_error("struct variant", work).await?);
        }
        Ok(child)
    }

    pub async fn unit(
        self,
        input: Input<'a>,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<()> {
        if let Some(value) = self.value {
            if value.kind() != JsonValueKind::Null {
                return Err(input.child(value).type_error("unit", work).await?);
            }
        }
        input.checkpoint(work).await
    }
    pub fn payload(&self, expected: &'static str) -> Result<JsonValue<'a>> {
        self.value
            .ok_or_else(|| JsonDiagnostic::unit_variant(expected, JsonPosition::unlocated()).into())
    }
}

pub(super) fn frame<'a, T, F>(control: ProductionControl<'a>, future: F) -> Result<Read<'a, T>>
where
    F: Future<Output = Result<Native<T>>> + Send + 'a,
    T: 'a,
{
    let memory = control
        .reserve(size_of_val(&future))
        .map_err(JsonReadError::from)?;
    Ok(Read {
        future: Box::pin(future),
        _memory: memory,
    })
}

pub(super) struct Cursor<'a, T> {
    values: Box<dyn Iterator<Item = T> + Send + 'a>,
    _memory: Option<MemoryReservation>,
}
impl<T> Iterator for Cursor<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
}
pub(super) fn iterator<'a, T: 'a>(
    control: ProductionControl<'_>,
    values: impl Iterator<Item = T> + Send + 'a,
) -> Result<Cursor<'a, T>> {
    let memory = control
        .reserve(size_of_val(&values))
        .map_err(JsonReadError::from)?;
    Ok(Cursor {
        values: Box::new(values),
        _memory: memory,
    })
}

pub(super) fn native<T>(value: Produced<T>, control: ProductionControl<'_>) -> Result<Native<T>> {
    Native::new(value, control).map_err(stored_error)
}

pub(super) fn finish<T>(parts: Parts<'_>, value: T) -> Result<Native<T>> {
    parts.finish(value).map_err(stored_error)
}

fn stored_error(error: InputError) -> JsonInputError {
    match error {
        InputError::Invalid => JsonInputError::Invalid,
        InputError::Resource(error) => JsonReadError::from(error).into(),
    }
}

fn enum_map() -> JsonInputError {
    JsonDiagnostic::message(
        "invalid value: map, expected map with a single key",
        JsonPosition::unlocated(),
    )
    .into()
}

#[cfg(test)]
mod tests;
