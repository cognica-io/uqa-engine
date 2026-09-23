//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned value copies reserve their destination buffers before producing payloads.

use std::collections::BTreeMap;

use crate::{
    memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryReservation},
    ArrayValue, CancellationToken, Value, ValueRetentionError,
};

impl Value {
    /// Copy the value under one allowance, retaining the destination's payload lease. Buffer capacity, array headers, decimal payloads and live map entries follow `reserve_retained_payload`; opaque B-tree node slack and allocator bookkeeping have the same separate ownership. Traversal uses a charged stack and checks cancellation between children and bounded string/byte chunks.
    pub fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Budgeted<Self>, ValueRetentionError> {
        copy(self, budget, &mut || {
            cancellation.check().map_err(Into::into)
        })
    }
}

struct Copier<'a> {
    budget: &'a MemoryBudget,
    check: &'a mut dyn FnMut() -> Result<(), ValueRetentionError>,
}

fn copy(
    source: &Value,
    budget: &MemoryBudget,
    check: &mut dyn FnMut() -> Result<(), ValueRetentionError>,
) -> Result<Budgeted<Value>, ValueRetentionError> {
    let mut copier = Copier { budget, check };
    let mut stack = BudgetedVec::<Frame<'_>>::new(budget);
    let mut next = Some(source);
    let mut ready = None;
    loop {
        (copier.check)()?;
        if let Some(source) = next.take() {
            match Frame::new(source, &mut copier)? {
                Some(mut frame) => {
                    if let Some(child) = frame.next(&mut copier)? {
                        stack.push(frame)?;
                        next = Some(child);
                        continue;
                    }
                    ready = Some(frame.finish(&mut copier)?);
                }
                None => ready = Some(copier.scalar(source)?),
            }
        }
        if let Some(value) = ready.take() {
            let Some(parent) = stack.last_mut() else {
                (copier.check)()?;
                return Ok(value);
            };
            parent.push(value)?;
        }
        if let Some(frame) = stack.last_mut() {
            if let Some(child) = frame.next(&mut copier)? {
                next = Some(child);
            } else {
                ready = Some(
                    stack
                        .pop()
                        .expect("active copy frame")
                        .finish(&mut copier)?,
                );
            }
        }
    }
}

enum Input<'a> {
    Values(std::slice::Iter<'a, Value>),
    Record(std::slice::Iter<'a, (String, Value)>),
    Map(std::collections::btree_map::Iter<'a, String, Value>),
}

enum Sequence<'a> {
    List,
    Row,
    Array(&'a ArrayValue),
}

enum Output<'a> {
    Values(BudgetedVec<Value>, Sequence<'a>),
    Record(BudgetedVec<(String, Value)>),
    Map(BTreeMap<String, Value>),
}

struct Frame<'a> {
    input: Input<'a>,
    output: Output<'a>,
    key: Option<Budgeted<String>>,
    memory: MemoryReservation,
}

impl<'a> Frame<'a> {
    fn new(
        source: &'a Value,
        copier: &mut Copier<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        let (input, output) = match source {
            Value::List(values) | Value::Row(values) => {
                let kind = if matches!(source, Value::List(_)) {
                    Sequence::List
                } else {
                    Sequence::Row
                };
                let mut output = BudgetedVec::new(copier.budget);
                output.reserve(values.len())?;
                (Input::Values(values.iter()), Output::Values(output, kind))
            }
            Value::Array(array) => {
                let mut output = BudgetedVec::new(copier.budget);
                output.reserve(array.elements().len())?;
                (
                    Input::Values(array.elements().iter()),
                    Output::Values(output, Sequence::Array(array)),
                )
            }
            Value::Record(fields) => {
                let mut output = BudgetedVec::new(copier.budget);
                output.reserve(fields.len())?;
                (Input::Record(fields.iter()), Output::Record(output))
            }
            Value::Map(fields) => (Input::Map(fields.iter()), Output::Map(BTreeMap::new())),
            _ => return Ok(None),
        };
        Ok(Some(Self {
            input,
            output,
            key: None,
            memory: copier.budget.empty_reservation(),
        }))
    }

    fn next(&mut self, copier: &mut Copier<'_>) -> Result<Option<&'a Value>, ValueRetentionError> {
        (copier.check)()?;
        let (name, value) = match &mut self.input {
            Input::Values(values) => return Ok(values.next()),
            Input::Record(fields) => fields.next().map(|(name, value)| (name.as_str(), value)),
            Input::Map(fields) => fields.next().map(|(name, value)| (name.as_str(), value)),
        }
        .unzip();
        if let Some(name) = name {
            if matches!(self.output, Output::Map(_)) {
                self.memory.grow(size_of::<(String, Value)>())?;
            }
            self.key = Some(copier.text(name)?);
        }
        Ok(value)
    }

    fn push(&mut self, value: Budgeted<Value>) -> Result<(), ValueRetentionError> {
        match &mut self.output {
            Output::Values(values, _) => {
                values.reserve(1)?;
                let (value, memory) = value.into_parts();
                values.push(value)?;
                self.memory.absorb(memory);
            }
            Output::Record(fields) => {
                fields.reserve(1)?;
                let (name, name_memory) = self.key.take().expect("copied record name").into_parts();
                let (value, memory) = value.into_parts();
                fields.push((name, value))?;
                self.memory.absorb(name_memory);
                self.memory.absorb(memory);
            }
            Output::Map(fields) => {
                let (name, name_memory) = self.key.take().expect("copied map name").into_parts();
                let (value, memory) = value.into_parts();
                fields.insert(name, value);
                self.memory.absorb(name_memory);
                self.memory.absorb(memory);
            }
        }
        Ok(())
    }

    fn finish(mut self, copier: &mut Copier<'_>) -> Result<Budgeted<Value>, ValueRetentionError> {
        (copier.check)()?;
        let value = match self.output {
            Output::Values(values, kind) => {
                let (values, memory) = values.into_parts();
                self.memory.absorb(memory);
                match kind {
                    Sequence::List => Value::List(values),
                    Sequence::Row => Value::Row(values),
                    Sequence::Array(source) => {
                        let dimensions = copier.slice(source.dimensions())?;
                        let lower_bounds = copier.slice(source.lower_bounds())?;
                        self.memory.grow(ArrayValue::decoded_header_bytes())?;
                        let (dimensions, dimensions_memory) = dimensions.into_parts();
                        let (lower_bounds, lower_memory) = lower_bounds.into_parts();
                        self.memory.absorb(dimensions_memory);
                        self.memory.absorb(lower_memory);
                        (copier.check)()?;
                        Value::Array(ArrayValue::from_copied_parts(
                            values,
                            dimensions,
                            lower_bounds,
                        ))
                    }
                }
            }
            Output::Record(fields) => {
                let (fields, memory) = fields.into_parts();
                self.memory.absorb(memory);
                Value::Record(fields)
            }
            Output::Map(fields) => Value::Map(fields),
        };
        Ok(Budgeted::new(value, self.memory))
    }
}

impl Copier<'_> {
    fn text(&mut self, text: &str) -> Result<Budgeted<String>, ValueRetentionError> {
        (self.check)()?;
        let mut copied = BudgetedString::new(self.budget);
        copied.reserve(text.len())?;
        let mut begin = 0;
        while begin < text.len() {
            (self.check)()?;
            let mut end = begin.saturating_add(4096).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            copied.push_str(&text[begin..end])?;
            begin = end;
        }
        (self.check)()?;
        let (value, memory) = copied.into_parts();
        Ok(Budgeted::new(value, memory))
    }

    fn slice<T: Copy>(&mut self, source: &[T]) -> Result<Budgeted<Vec<T>>, ValueRetentionError> {
        (self.check)()?;
        let mut copied = BudgetedVec::new(self.budget);
        copied.reserve(source.len())?;
        for chunk in source.chunks((4096 / size_of::<T>().max(1)).max(1)) {
            (self.check)()?;
            copied.extend_from_slice(chunk)?;
        }
        (self.check)()?;
        let (value, memory) = copied.into_parts();
        Ok(Budgeted::new(value, memory))
    }

    fn scalar(&mut self, source: &Value) -> Result<Budgeted<Value>, ValueRetentionError> {
        (self.check)()?;
        let mut memory = self.budget.empty_reservation();
        let value = match source {
            Value::Null => Value::Null,
            Value::Void => Value::Void,
            Value::Bool(value) => Value::Bool(*value),
            Value::Int(value) => Value::Int(*value),
            Value::Float(value) => Value::Float(*value),
            Value::Temporal(value) => Value::Temporal(value.clone()),
            Value::Str(text) | Value::FixedChar(text) | Value::Json(text) | Value::JsonB(text) => {
                let (text, charge) = self.text(text)?.into_parts();
                memory.absorb(charge);
                match source {
                    Value::Str(_) => Value::Str(text),
                    Value::FixedChar(_) => Value::FixedChar(text),
                    Value::Json(_) => Value::Json(text),
                    Value::JsonB(_) => Value::JsonB(text),
                    _ => unreachable!("text variant"),
                }
            }
            Value::Bytes(bytes) => {
                let (bytes, charge) = self.slice(bytes)?.into_parts();
                memory.absorb(charge);
                Value::Bytes(bytes)
            }
            Value::Decimal(value) => {
                memory.grow(value.retained_bytes())?;
                Value::Decimal(value.clone())
            }
            Value::Array(_) | Value::List(_) | Value::Row(_) | Value::Record(_) | Value::Map(_) => {
                unreachable!("container has a copy frame")
            }
        };
        let result = Budgeted::new(value, memory);
        (self.check)()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
