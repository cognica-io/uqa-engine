//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct native JSON values through the shared retained grammar and operation schedule.

use crate::work::Work;
use serde_json::Value;
use uqa_core::{
    image_io::ImageIoError,
    json::{
        decode_json_string_with_work, JsonDiagnostic, JsonPosition, JsonReadError, JsonReadWork,
        JsonReader, JsonToken,
    },
    memory::{Produced, ProductionControl},
};

mod comparison;
pub(crate) use comparison::{bytes_compare, bytes_equal};

mod output;
pub(crate) use output::{write_raw, write_string, write_value, Output};

pub(crate) fn fragment(text: &str, position: usize) -> &str {
    let mut end = position.saturating_add(4096).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[position..end]
}

mod value;

enum Error<E> {
    Grammar(JsonReadError),
    Serde(serde_json::Error),
    Diagnostic(JsonDiagnostic),
    Work(E),
}

impl<E> From<serde_json::Error> for Error<E> {
    fn from(error: serde_json::Error) -> Self {
        Self::Serde(error)
    }
}

impl<E> Error<E> {
    fn into_data(self) -> Self {
        match self {
            Self::Diagnostic(error) => Self::Diagnostic(error.into_data()),
            Self::Serde(error) => {
                Self::Serde(<serde_json::Error as serde::de::Error>::custom(error))
            }
            error => error,
        }
    }
}

impl<E> From<JsonReadError> for Error<E> {
    fn from(error: JsonReadError) -> Self {
        Self::Grammar(error)
    }
}

impl<E> From<JsonDiagnostic> for Error<E> {
    fn from(error: JsonDiagnostic) -> Self {
        Self::Diagnostic(error)
    }
}

struct Scheduler<'a, W, E>(&'a mut W, std::marker::PhantomData<fn() -> E>);

impl<W: Work<E>, E> JsonReadWork for Scheduler<'_, W, E> {
    type Error = Error<E>;

    async fn checkpoint(&mut self) -> Result<(), Error<E>> {
        self.0.checkpoint().await.map_err(Error::Work)
    }
}

pub(crate) trait InputError:
    From<serde_json::Error> + From<JsonDiagnostic> + From<ImageIoError> + Send
{
}
impl<E: From<serde_json::Error> + From<JsonDiagnostic> + From<ImageIoError> + Send> InputError
    for E
{
}

/// Borrowed syntax retains the enclosing schedule; Analysis owns decoded model allocations.
pub(crate) struct Input<'a> {
    reader: JsonReader<'a, 'static>,
}

impl<'a> Input<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            reader: JsonReader::from_slice_with_control(bytes, &ProductionControl::uncontrolled())
                .with_depth_limit(127),
        }
    }

    pub(crate) async fn next<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Option<JsonToken<'a, &'a [u8]>>, E> {
        self.next_native(work)
            .await
            .map_err(|error| self.error(error))
    }

    async fn next_native<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Option<JsonToken<'a, &'a [u8]>>, Error<E>> {
        match self
            .reader
            .next_event_with_work(&mut Scheduler(work, std::marker::PhantomData))
            .await
        {
            Ok(event) => Ok(event.map(|event| event.token)),
            Err(error) => Err(self.located(error)),
        }
    }

    pub(crate) fn position(&self) -> JsonPosition {
        self.reader.position()
    }

    fn located<E>(&self, error: Error<E>) -> Error<E> {
        match error {
            Error::Grammar(JsonReadError::InvalidJson | JsonReadError::Malformed { .. }) => {
                Error::Diagnostic(self.reader.syntax_error().map_or_else(
                    || JsonDiagnostic::message("invalid JSON text", self.position()),
                    JsonDiagnostic::from,
                ))
            }
            error => error,
        }
    }

    fn error<E: InputError>(&self, error: Error<E>) -> E {
        match self.located(error) {
            Error::Work(error) => error,
            Error::Serde(error) => error.into(),
            Error::Diagnostic(error) => error.into(),
            Error::Grammar(JsonReadError::Memory(error)) => ImageIoError::from(error).into(),
            Error::Grammar(JsonReadError::Cancelled(error)) => ImageIoError::from(error).into(),
            Error::Grammar(_) => unreachable!("native syntax already located"),
        }
    }

    pub(crate) async fn invalid_type<E: InputError>(
        &self,
        token: JsonToken<'_, &'_ [u8]>,
        expected: &'static str,
        work: &mut impl Work<E>,
    ) -> Result<E, E> {
        let owner = work.production_control();
        JsonDiagnostic::observe_type_with_work(
            token,
            self.position(),
            expected,
            &owner.control(),
            &mut Scheduler(work, std::marker::PhantomData),
        )
        .await
        .map(Into::into)
        .map_err(|error| self.error(error))
    }

    pub(crate) async fn name<E: InputError>(
        &self,
        encoded: &[u8],
        work: &mut impl Work<E>,
    ) -> Result<Produced<String>, E> {
        let owner = work.production_control();
        decode_json_string_with_work(
            encoded,
            &owner.control(),
            &mut Scheduler(work, std::marker::PhantomData),
        )
        .await
        .map_err(|error| self.error(error))
    }

    pub(crate) async fn text<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<String, E> {
        let token = self.next(work).await?.expect("typed input expects a value");
        match token {
            JsonToken::String(encoded) => self.string(encoded, work).await,
            token => Err(self.invalid_type(token, "a string", work).await?),
        }
    }

    pub(crate) async fn u32<E: InputError>(&mut self, work: &mut impl Work<E>) -> Result<u32, E> {
        let owner = work.production_control();
        self.reader
            .read_u32_with_work(
                &owner.control(),
                &mut Scheduler(work, std::marker::PhantomData),
            )
            .await
            .map_err(|error| self.error(error))
    }

    pub(crate) async fn u8<E: InputError>(&mut self, work: &mut impl Work<E>) -> Result<u8, E> {
        let owner = work.production_control();
        self.reader
            .read_u8_with_work(
                &owner.control(),
                &mut Scheduler(work, std::marker::PhantomData),
            )
            .await
            .map_err(|error| self.error(error))
    }

    pub(crate) async fn unit_variant<E: InputError>(
        &mut self,
        variants: &'static [&'static str],
        work: &mut impl Work<E>,
    ) -> Result<usize, E> {
        let owner = work.production_control();
        self.reader
            .read_unit_variant_with_work(
                variants,
                &owner.control(),
                &mut Scheduler(work, std::marker::PhantomData),
            )
            .await
            .map_err(|error| self.error(error))
    }

    pub(crate) async fn next_array_value<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<bool, E> {
        self.reader
            .next_array_value_with_work(&mut Scheduler(work, std::marker::PhantomData))
            .await
            .map_err(|error| self.error(error))
    }

    pub(crate) async fn finish_array<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<(), E> {
        self.reader
            .finish_array_with_work(&mut Scheduler(work, std::marker::PhantomData))
            .await
            .map_err(|error| self.error(error))
    }

    pub(crate) async fn finish_struct_error<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<(), E> {
        self.reader
            .finish_struct_error_with_work(&mut Scheduler(work, std::marker::PhantomData))
            .await
            .map(|_| ())
            .map_err(|error| self.error(error))
    }

    pub(crate) async fn string<E: InputError>(
        &mut self,
        encoded: &[u8],
        work: &mut impl Work<E>,
    ) -> Result<String, E> {
        self.model_string(encoded, work)
            .await
            .map_err(|error| self.error(error))
    }

    async fn model_string<E: InputError>(
        &self,
        encoded: &[u8],
        work: &mut impl Work<E>,
    ) -> Result<String, Error<E>> {
        decode_json_string_with_work(
            encoded,
            &ProductionControl::uncontrolled(),
            &mut Scheduler(work, std::marker::PhantomData),
        )
        .await
        .map(|value| {
            value
                .into_uncontrolled()
                .expect("Analysis owns model allocations")
        })
        .map_err(|error| self.located(error))
    }

    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    pub(crate) async fn root<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Value, E> {
        let value = self.value(work).await?;
        if self.next(work).await?.is_some() {
            unreachable!("the grammar rejects multiple root values");
        }
        Ok(value)
    }

    pub(crate) async fn value<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Value, E> {
        let result = match value::read(self, work) {
            Ok(value) => {
                let (future, memory) = value.into_parts();
                let result = future.await;
                drop(memory);
                result
            }
            Err(error) => Err(error),
        };
        result.map_err(|error| self.error(error))
    }
}
