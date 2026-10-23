//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled construction preserves the ordinary value visitor's numeric and tagged representations.

use std::collections::BTreeMap;

use crate::{
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec, MemoryBudget},
    CancellationToken, DecimalValue, ValueRetentionError,
};

use super::{tagged::value_from_tagged_map_budgeted, Value};

mod buffers;
mod root;
use buffers::{ArrayBuffer, FieldBuffer};
use root::FieldName;

type FieldVisitor<'a> = dyn FnMut(&str, Budgeted<Value>) -> Result<(), JsonReadError> + 'a;

/// Decode canonical values while reserving owned containers, strings and tagged payloads before allocation. Returned values retain their payload allowance; parser and conversion scratch is released before returning.
pub struct JsonValueDecoder<'a> {
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    depth_limit: usize,
}

enum Container {
    Array(ArrayBuffer),
    Object(FieldBuffer),
    VisitedFields,
}

impl Value {
    /// Apply the ordinary JSON visitor's private-number and tagged-object rules to decoded fields. The input reservation must cover its live entries, key capacities and nested payloads; conversion retains that allowance and moves recognized buffers.
    pub fn from_json_fields_budgeted(
        fields: Budgeted<BTreeMap<String, Value>>,
        cancellation: &CancellationToken,
    ) -> Result<Budgeted<Self>, JsonReadError> {
        let (fields, reservation) = fields.into_parts();
        let memory = reservation.budget().clone();
        JsonValueDecoder::new(&memory, cancellation).object(Budgeted::new(fields, reservation))
    }
}

impl<'a> JsonValueDecoder<'a> {
    pub fn new(memory: &'a MemoryBudget, cancellation: &'a CancellationToken) -> Self {
        Self {
            memory,
            cancellation,
            depth_limit: 127,
        }
    }

    /// Reduce the remaining container allowance when an owning format has already decoded outer containers. The ordinary serde value limit remains the maximum.
    #[must_use]
    pub fn with_depth_limit(mut self, depth_limit: usize) -> Self {
        self.depth_limit = depth_limit.min(127);
        self
    }

    pub fn value(&self, text: &str) -> Result<Budgeted<Value>, JsonReadError> {
        self.read(text, false, None)
    }

    /// Decode an object whose root keys are ordinary fields, including reserved tagged-value names. Nested field values keep normal tagged-value recognition.
    pub fn fields(&self, text: &str) -> Result<Budgeted<BTreeMap<String, Value>>, JsonReadError> {
        let (value, memory) = self.read(text, true, None)?.into_parts();
        let Value::Map(fields) = value else {
            unreachable!("field root is an object");
        };
        Ok(Budgeted::new(fields, memory))
    }

    /// Visit every root field occurrence in input order without constructing a
    /// root field map. Unescaped names borrow the input; nested values use the
    /// ordinary decoder, including conversion of overwritten or unselected
    /// fields. A collecting visitor must replace duplicate names and discard
    /// its staged output if decoding fails. Each value carries its own lease.
    pub fn visit_fields(
        &self,
        text: &str,
        visitor: &mut FieldVisitor<'_>,
    ) -> Result<(), JsonReadError> {
        self.read(text, true, Some(visitor)).map(|_| ())
    }

    fn scalar(&self, value: Value) -> Budgeted<Value> {
        Budgeted::new(value, self.memory.empty_reservation())
    }

    fn number(&self, text: &str) -> Result<Budgeted<Value>, JsonReadError> {
        self.cancellation.check()?;
        if let Some(value) = primitive_number(text) {
            return Ok(self.scalar(value));
        }
        let decimal = DecimalValue::parse_budgeted(text, self.memory, self.cancellation)?
            .ok_or(JsonReadError::InvalidJson)?;
        let (decimal, memory) = decimal.into_parts();
        Ok(Budgeted::new(Value::Decimal(decimal), memory))
    }

    fn object(
        &self,
        fields: Budgeted<BTreeMap<String, Value>>,
    ) -> Result<Budgeted<Value>, JsonReadError> {
        if fields.len() == 1 {
            if let Some(Value::Str(text)) = fields.get("$serde_json::private::Number") {
                return self.number(text);
            }
        }
        value_from_tagged_map_budgeted(fields, self.cancellation).map_err(Into::into)
    }

    fn read(
        &self,
        text: &str,
        field_root: bool,
        mut visitor: Option<&mut FieldVisitor<'_>>,
    ) -> Result<Budgeted<Value>, JsonReadError> {
        self.cancellation.check()?;
        let mut reader = JsonReader::new(text, self.memory, self.cancellation)
            .with_depth_limit(self.depth_limit);
        let mut stack = BudgetedVec::new(self.memory);
        let mut root = None;
        let mut root_key = None;
        let mut first = true;
        while let Some(event) = reader.next_event()? {
            if first && field_root && event.token != JsonToken::StartObject {
                return Err(JsonReadError::InvalidJson);
            }
            first = false;
            let value = match event.token {
                JsonToken::Null => self.scalar(Value::Null),
                JsonToken::Bool(value) => self.scalar(Value::Bool(value)),
                JsonToken::Number(text) => {
                    // serde_json's arbitrary-precision visitor first emits an unsigned primitive when the complete token fits u64.
                    if let Ok(value) = text.parse::<u64>() {
                        self.scalar(
                            i64::try_from(value).map_or(Value::Float(value as f64), Value::Int),
                        )
                    } else {
                        self.number(text)?
                    }
                }
                JsonToken::String(text) => {
                    let (text, memory) =
                        decode_json_string(text, self.memory, self.cancellation)?.into_parts();
                    Budgeted::new(Value::Str(text), memory)
                }
                JsonToken::Key(text) => {
                    match stack.last_mut() {
                        Some(Container::VisitedFields) => {
                            root_key = Some(FieldName::read(text, self)?);
                        }
                        Some(Container::Object(fields)) => {
                            fields.key(decode_json_string(text, self.memory, self.cancellation)?);
                        }
                        _ => unreachable!("object key event"),
                    }
                    continue;
                }
                JsonToken::StartArray => {
                    stack.push(Container::Array(ArrayBuffer::new(self.memory)))?;
                    continue;
                }
                JsonToken::StartObject => {
                    stack.push(if stack.is_empty() && visitor.is_some() {
                        Container::VisitedFields
                    } else {
                        Container::Object(FieldBuffer::new(self.memory))
                    })?;
                    continue;
                }
                JsonToken::EndArray => {
                    let Some(Container::Array(values)) = stack.pop() else {
                        unreachable!("array end event");
                    };
                    values.finish()
                }
                JsonToken::EndObject => match stack.pop() {
                    Some(Container::VisitedFields) => self.scalar(Value::Null),
                    Some(Container::Object(fields)) => {
                        let fields = fields.finish(self.cancellation)?;
                        if field_root && stack.is_empty() {
                            let (fields, memory) = fields.into_parts();
                            Budgeted::new(Value::Map(fields), memory)
                        } else {
                            self.object(fields)?
                        }
                    }
                    _ => unreachable!("object end event"),
                },
            };
            match stack.last_mut() {
                Some(Container::Array(values)) => values.push(value)?,
                Some(Container::Object(fields)) => fields.push(value)?,
                Some(Container::VisitedFields) => {
                    let name = root_key.take().expect("root value follows its key");
                    visitor.as_mut().expect("field visitor")(name.as_str(), value)?;
                }
                None => root = Some(value),
            }
        }
        self.cancellation.check()?;
        root.ok_or(JsonReadError::InvalidJson)
    }
}

pub(super) fn primitive_number(text: &str) -> Option<Value> {
    if let Ok(integer) = text.parse::<i64>() {
        return Some(Value::Int(integer));
    }
    text.parse::<f64>()
        .ok()
        .filter(|float| float.is_finite())
        .map(Value::Float)
}

impl From<ValueRetentionError> for JsonReadError {
    fn from(error: ValueRetentionError) -> Self {
        match error {
            ValueRetentionError::Memory(error) => Self::Memory(error),
            ValueRetentionError::Cancelled(error) => Self::Cancelled(error),
            ValueRetentionError::Malformed { kind, reason } => Self::Malformed { kind, reason },
        }
    }
}

#[cfg(test)]
mod tests;
