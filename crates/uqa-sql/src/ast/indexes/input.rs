//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete original key validation precedes projection of ordinary column names.

use super::IndexKey;
use crate::ast::{
    expressions::validate_scalar_fields,
    stored_input::{invalid, number, unit, Input, InputError, Values},
};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{
        decode_json_string_from, JsonDiagnostic, JsonInputError, JsonReadError, JsonReader,
        JsonToken,
    },
    memory::{Budgeted, BudgetedVec},
};

mod shapes;
use shapes::{Field, Kind, Shape};
mod queries;

impl IndexKey {
    /// Validate the complete stored key grammar, including ignored query/window expressions, and retain only ordinary string-column keys in their original order. Expression-valued column references remain expressions. This performs no SQL evaluation or catalog binding.
    pub async fn read_stored_columns_from(
        text: &str,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<BudgetedVec<Budgeted<String>>, JsonInputError> {
        read(text, work).await
    }
}

async fn read(
    text: &str,
    work: &mut AsyncImageInput<'_>,
) -> Result<BudgetedVec<Budgeted<String>>, JsonInputError> {
    let mut reader = JsonReader::new(text, work.memory, work.cancellation).with_depth_limit(127);
    let first = reader
        .next_event_from(work)
        .await
        .map_err(|error| JsonInputError::from_reader(&reader, error))?
        .ok_or(JsonInputError::Invalid)?;
    if first.token != JsonToken::StartArray {
        return Err(JsonDiagnostic::invalid_type_from(
            first.token,
            reader.position(),
            "a sequence",
            work,
        )
        .await?
        .into());
    }
    let mut columns = BudgetedVec::new(work.memory);
    loop {
        let first = reader
            .next_event_from(work)
            .await
            .map_err(|error| JsonInputError::from_reader(&reader, error))?
            .ok_or(JsonInputError::Invalid)?;
        if first.token == JsonToken::EndArray {
            break;
        }
        if let JsonToken::String(encoded) = first.token {
            columns.push(decode_json_string_from(encoded, work).await?)?;
            continue;
        }
        let start = first.range.start;
        let mut end = first.range.end;
        let mut depth = usize::from(matches!(
            first.token,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            let event = reader
                .next_event_from(work)
                .await
                .map_err(|error| JsonInputError::from_reader(&reader, error))?
                .ok_or(JsonInputError::Invalid)?;
            match event.token {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
            end = event.range.end;
        }
        if let Err(error) = validate(&text[start..end], work).await {
            use uqa_core::ValueRetentionError;
            return Err(match error {
                InputError::Resource(ValueRetentionError::Memory(error)) => error.into(),
                InputError::Resource(ValueRetentionError::Cancelled(error)) => error.into(),
                InputError::Invalid
                | InputError::Resource(ValueRetentionError::Malformed { .. }) => {
                    JsonDiagnostic::message(
                        "data did not match any variant of untagged enum IndexKey",
                        reader.finish_sequence_error_from(work).await?,
                    )
                    .into()
                }
            });
        }
    }
    if reader
        .next_event_from(work)
        .await
        .map_err(|error| JsonInputError::from_reader(&reader, error))?
        .is_some()
    {
        return Err(JsonInputError::Invalid);
    }
    work.checkpoint().await?;
    Ok(columns)
}

#[derive(Clone, Copy)]
struct Value<'a> {
    text: &'a str,
    depth: usize,
}

enum Task<'a> {
    Read(Shape, Value<'a>),
    Alternative(Shape, Value<'a>),
}

struct Pending<'a> {
    tasks: BudgetedVec<Task<'a>>,
}

impl<'a> Pending<'a> {
    fn value(&mut self, shape: Shape, value: Value<'a>) -> Result<(), InputError> {
        self.tasks.push(Task::Read(shape, value))?;
        Ok(())
    }

    fn token(
        &mut self,
        shape: Shape,
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
    ) -> Result<(), InputError> {
        let (text, depth) = input.encoded(token)?;
        self.value(shape, Value { text, depth })
    }

    fn read(
        &mut self,
        shape: Shape,
        value: Value<'a>,
        values: &Values<'_>,
    ) -> Result<(), InputError> {
        if let Shape::Node(kind) = shape {
            return match kind {
                Kind::Expression => self.expression(value, values),
                Kind::Cte => self.cte(value, values),
                kind => self.read(shapes::node(kind), value, values),
            };
        }
        if let Shape::Choice(first, second) = shape {
            self.tasks.push(Task::Alternative(*second, value))?;
            return self.value(*first, value);
        }
        let mut input = Input::content_at_depth(value.text, values.control(), value.depth);
        let token = input.next()?;
        match shape {
            Shape::Text if matches!(token, JsonToken::String(_)) => (),
            Shape::Bool if matches!(token, JsonToken::Bool(_)) => (),
            Shape::Null if token == JsonToken::Null => (),
            Shape::Scalar(read) => read(&mut input, token)?,
            Shape::Binding => {
                input.function_binding(token)?;
            }
            Shape::ColumnType => {
                input.column(token)?;
            }
            Shape::Optional(shape) => {
                if token != JsonToken::Null {
                    self.token(*shape, &mut input, token)?;
                }
            }
            Shape::Array(shape) => {
                input.visit_array(token, |input, token| self.token(*shape, input, token))?;
            }
            Shape::Tuple(shapes) => {
                let mut index = 0;
                input.visit_array(token, |input, token| {
                    let shape = shapes.get(index).ok_or_else(invalid)?;
                    self.token(*shape, input, token)?;
                    index += 1;
                    Ok(())
                })?;
                if index != shapes.len() {
                    return Err(invalid());
                }
            }
            Shape::Bytes => {
                let mut count = 0;
                input.visit_array(token, |_, token| {
                    number::<u8>(token)?;
                    count += 1;
                    Ok(())
                })?;
                if count != 16 {
                    return Err(invalid());
                }
            }
            Shape::Fields(fields) => self.fields(fields, &mut input, token)?,
            Shape::Map(shape) => {
                input.object(token, |input, _, token| self.token(*shape, input, token))?;
            }
            Shape::Enum(variants) => {
                let (tag, object) = input.tag(token)?;
                let (_, body) = variants
                    .iter()
                    .find(|(name, _)| *name == tag.as_str())
                    .ok_or_else(invalid)?;
                if object {
                    let token = input.next()?;
                    self.token(*body, &mut input, token)?;
                } else if !matches!(body, Shape::Null) {
                    return Err(invalid());
                }
                input.close_tag(object)?;
            }
            _ => return Err(invalid()),
        }
        input.end()
    }

    fn fields(
        &mut self,
        fields: &[Field],
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
    ) -> Result<(), InputError> {
        let mut names = [""; 32];
        let (mut required, mut sequence_required) = (0, 0);
        for (index, field) in fields.iter().enumerate() {
            names[index] = field.name;
            required |= u64::from(field.required) << index;
            sequence_required |= u64::from(field.sequence_required) << index;
        }
        input.fields_with_defaults(
            token,
            &names[..fields.len()],
            required,
            sequence_required,
            |input, index, token| self.token(fields[index].shape, input, token),
        )
    }

    fn expression(&mut self, value: Value<'a>, values: &Values<'_>) -> Result<(), InputError> {
        let mut input = Input::content_at_depth(value.text, values.control(), value.depth);
        let token = input.next()?;
        let (tag, object) = input.tag(token)?;
        let shape = match tag.as_str() {
            "ScalarSubquery" => Shape::Node(Kind::Select),
            "Exists" => shapes::EXISTS,
            "InSubquery" => shapes::IN_SUBQUERY,
            "WindowCall" => shapes::WINDOW_CALL,
            _ => {
                drop(tag);
                return validate_scalar_fields(value.text, value.depth, values, |text, depth| {
                    self.value(Shape::Node(Kind::Expression), Value { text, depth })
                });
            }
        };
        if !object {
            return Err(invalid());
        }
        let token = input.next()?;
        self.token(shape, &mut input, token)?;
        input.close_tag(object)?;
        input.end()
    }
}

async fn validate(text: &str, work: &mut AsyncImageInput<'_>) -> Result<(), InputError> {
    let values = Values {
        memory: work.memory,
        original: work.cancellation,
        invoking: work.cancellation,
    };
    let mut pending = Pending {
        tasks: BudgetedVec::new(work.memory),
    };
    pending.value(Shape::Node(Kind::Expression), Value { text, depth: 127 })?;
    while let Some(task) = pending.tasks.pop() {
        work.checkpoint().await.map_err(JsonReadError::from)?;
        let Task::Read(shape, value) = task else {
            continue;
        };
        match pending.read(shape, value, &values) {
            Ok(()) => (),
            Err(InputError::Resource(error)) => return Err(InputError::Resource(error)),
            Err(InputError::Invalid) => loop {
                match pending.tasks.pop() {
                    Some(Task::Alternative(shape, value)) => {
                        pending.value(shape, value)?;
                        break;
                    }
                    Some(Task::Read(_, _)) => {
                        work.checkpoint().await.map_err(JsonReadError::from)?;
                    }
                    None => return Err(invalid()),
                }
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
