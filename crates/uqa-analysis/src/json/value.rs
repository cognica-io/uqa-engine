//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analysis owns native value allocations; Core retains lexical and public scalar boundaries.

use super::{Error, Input, InputError, JsonReadError, JsonToken, Produced, Scheduler, Value, Work};
use std::{future::Future, pin::Pin};
use uqa_core::json::{read_json_number_text, JsonScalarProtocol};

enum Kind {
    Number,
    Raw,
}

enum Frame {
    Array(Vec<Value>),
    Object(serde_json::Map<String, Value>, Option<String>),
    Scalar(Kind),
}

impl Input<'_> {
    pub(super) async fn native_root<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Value, Error<E>> {
        let value = self.native_value(work).await?;
        if self.next_native(work).await?.is_some() {
            unreachable!("the grammar rejects multiple root values");
        }
        Ok(value)
    }

    pub(super) async fn native_value<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Value, Error<E>> {
        let protocol = JsonScalarProtocol::from_values(
            &serde_json::Number::from(1u8),
            serde_json::value::RawValue::NULL,
        )?;
        let mut stack = Vec::new();
        loop {
            let value = if matches!(stack.last(), Some(Frame::Scalar(_))) {
                let Some(Frame::Scalar(kind)) = stack.pop() else {
                    unreachable!()
                };
                self.scalar(kind, work).await?
            } else {
                let token = self
                    .next_native(work)
                    .await?
                    .expect("generic input expects one value");
                match token {
                    JsonToken::StartArray => {
                        stack.push(Frame::Array(Vec::new()));
                        continue;
                    }
                    JsonToken::StartObject => {
                        stack.push(Frame::Object(serde_json::Map::new(), None));
                        continue;
                    }
                    JsonToken::Key(encoded) => {
                        let name = self.model_string(encoded, work).await?;
                        let Some(Frame::Object(values, pending)) = stack.last_mut() else {
                            unreachable!("object key")
                        };
                        let kind = if values.is_empty() && name == protocol.number {
                            Some(Kind::Number)
                        } else if values.is_empty() && name == protocol.raw {
                            Some(Kind::Raw)
                        } else {
                            None
                        };
                        if let Some(kind) = kind {
                            *stack.last_mut().expect("object") = Frame::Scalar(kind);
                        } else {
                            *pending = Some(name);
                        }
                        continue;
                    }
                    JsonToken::EndArray => {
                        let Some(Frame::Array(values)) = stack.pop() else {
                            unreachable!("matching array")
                        };
                        Value::Array(values)
                    }
                    JsonToken::EndObject => {
                        let Some(Frame::Object(values, None)) = stack.pop() else {
                            unreachable!("complete object")
                        };
                        Value::Object(values)
                    }
                    JsonToken::Null => Value::Null,
                    JsonToken::Bool(value) => Value::Bool(value),
                    JsonToken::String(encoded) => {
                        Value::String(self.model_string(encoded, work).await?)
                    }
                    // The public numeric constructor retains its existing atomic conversion boundary.
                    JsonToken::Number(encoded) => Value::Number(serde_json::from_slice(encoded)?),
                }
            };
            match stack.last_mut() {
                Some(Frame::Array(values)) => values.push(value),
                Some(Frame::Object(values, key)) => {
                    values.insert(key.take().expect("original object key"), value);
                }
                Some(Frame::Scalar(_)) => unreachable!("scalar payload is decoded separately"),
                None => return Ok(value),
            }
        }
    }

    async fn scalar<E: InputError>(
        &mut self,
        kind: Kind,
        work: &mut impl Work<E>,
    ) -> Result<Value, Error<E>> {
        let owner = work.production_control();
        let control = owner.control();
        let expected = match kind {
            Kind::Number => "string containing a number",
            Kind::Raw => "raw value",
        };
        let text = self
            .reader
            .read_string_with_work(
                expected,
                &control,
                &mut Scheduler(work, std::marker::PhantomData),
            )
            .await
            .map_err(|error| self.located(error))?;
        let value = match kind {
            Kind::Number => {
                read_json_number_text(
                    &text,
                    &control,
                    &mut Scheduler(work, std::marker::PhantomData),
                )
                .await
                .map_err(Error::into_data)?;
                Value::Number(text.parse()?)
            }
            Kind::Raw => {
                let (future, lease) = raw(&text, work)?.into_parts();
                let value = future.await.map_err(Error::into_data)?;
                drop(lease);
                value
            }
        };
        self.reader
            .finish_scalar_object(&mut Scheduler(work, std::marker::PhantomData))
            .await
            .map_err(|error| self.located(error))?;
        Ok(value)
    }
}

type Raw<'a, E> = Produced<Pin<Box<dyn Future<Output = Result<Value, Error<E>>> + Send + 'a>>>;

/// A native value continuation retains its own layout and allowance independently of enclosing dictionary and descriptor jobs.
pub(super) fn read<'a, E: InputError + 'a, W: Work<E> + 'a>(
    input: &'a mut Input<'_>,
    work: &'a mut W,
) -> Result<Raw<'a, E>, Error<E>> {
    let owner = work.production_control();
    retain(
        &owner.control(),
        async move { input.native_value(work).await },
    )
}

fn retain<'a, E: InputError + 'a>(
    control: &super::ProductionControl<'_>,
    future: impl Future<Output = Result<Value, Error<E>>> + Send + 'a,
) -> Result<Raw<'a, E>, Error<E>> {
    let memory = control
        .reserve(size_of_val(&future))
        .map_err(JsonReadError::from)?;
    let future: Pin<Box<dyn Future<Output = Result<Value, Error<E>>> + Send + 'a>> =
        Box::pin(future);
    control
        .finish(future, memory)
        .map_err(|error| JsonReadError::from(error).into())
}

fn raw<'a, E: InputError + 'a, W: Work<E> + 'a>(
    text: &'a str,
    work: &'a mut W,
) -> Result<Raw<'a, E>, Error<E>> {
    let owner = work.production_control();
    let control = owner.control();
    retain(&control, async move {
        Input::new(text.as_bytes()).native_root(work).await
    })
}
