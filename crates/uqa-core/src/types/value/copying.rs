//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned value copies reserve their destination buffers before producing payloads.

use std::collections::BTreeMap;

use crate::{
    memory::{
        Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, Produced, ProductionControl,
        ProductionString, ProductionVec,
    },
    ArrayValue, CancellationToken, LegacyVectorKind, LegacyVectorValue, QueryCancelled, RowValue,
    Value, ValueRetentionError,
};

impl Value {
    /// Copy the value under one allowance, retaining the destination's payload lease. Buffer capacity, array headers, decimal payloads and live map entries follow `reserve_retained_payload`; opaque B-tree node slack and allocator bookkeeping have the same separate ownership. Traversal uses a charged stack and checks cancellation between children and bounded string/byte chunks.
    pub fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Budgeted<Self>, ValueRetentionError> {
        self.clone_budgeted_with_check(budget, || cancellation.check())
    }

    /// Copy under the same payload and traversal contract as [`Self::clone_budgeted`], checking all caller-owned cancellation scopes through one callback between children and bounded payload chunks. This supports retained resources whose original caller and current reader both remain cancellation owners.
    pub fn clone_budgeted_with_check(
        &self,
        budget: &MemoryBudget,
        mut check: impl FnMut() -> Result<(), QueryCancelled>,
    ) -> Result<Budgeted<Self>, ValueRetentionError> {
        copy(
            self,
            ProductionControl::with_budget(budget),
            &mut || check().map_err(Into::into),
            &mut |_| Ok::<_, ValueRetentionError>(None),
        )
        .map(|value| value.into_budgeted().expect("controlled value copy"))
    }

    /// Copy native containers with an iterative, admitted traversal. A replacement owns its admitted payload under the same control and stops traversal at that node; all other nodes preserve their shape, bounds and descriptors. Replacements are never interpreted by Core. A failed transform releases every partial copy.
    pub fn transform_with_control<E: From<ValueRetentionError>>(
        &self,
        control: &ProductionControl<'_>,
        mut replace: impl FnMut(&Value) -> Result<Option<Produced<Value>>, E>,
    ) -> Result<Produced<Value>, E> {
        copy(self, *control, &mut || control.check(), &mut replace)
    }
}

struct Copier<'a> {
    control: ProductionControl<'a>,
    check: &'a mut dyn FnMut() -> Result<(), ValueRetentionError>,
}

fn copy<E: From<ValueRetentionError>>(
    source: &Value,
    control: ProductionControl<'_>,
    check: &mut dyn FnMut() -> Result<(), ValueRetentionError>,
    replace: &mut impl FnMut(&Value) -> Result<Option<Produced<Value>>, E>,
) -> Result<Produced<Value>, E> {
    let mut copier = Copier { control, check };
    let mut stack = Stack::new(control.budget());
    let mut next = Some(source);
    let mut ready = None;
    loop {
        (copier.check)()?;
        if let Some(source) = next.take() {
            if let Some(value) = replace(source)? {
                let (value, memory) = value.into_parts();
                ready = Some(control.finish(value, memory)?);
            } else {
                match Frame::new(source, &mut copier)? {
                    Some(mut frame) => {
                        if let Some(child) = frame.next(&mut copier)? {
                            stack.push(frame).map_err(ValueRetentionError::from)?;
                            next = Some(child);
                            continue;
                        }
                        ready = Some(frame.finish(&mut copier)?);
                    }
                    None => ready = Some(copier.scalar(source)?),
                }
            }
        }
        if let Some(value) = ready.take() {
            let Some(parent) = stack.last_mut() else {
                (copier.check)()?;
                return Ok(value);
            };
            parent.push(value, &mut copier)?;
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

// Frame payloads own their own leases; this stack admits only its frame buffer.
enum Stack<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> Stack<T> {
    fn new(budget: Option<&MemoryBudget>) -> Self {
        budget.map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Controlled(BudgetedVec::new(budget)),
        )
    }
    fn push(&mut self, value: T) -> Result<(), crate::memory::MemoryError> {
        match self {
            Self::Ordinary(values) => {
                values
                    .try_reserve(1)
                    .map_err(crate::memory::MemoryError::from)?;
                values.push(value);
                Ok(())
            }
            Self::Controlled(values) => values.push(value),
        }
    }
    fn pop(&mut self) -> Option<T> {
        match self {
            Self::Ordinary(values) => values.pop(),
            Self::Controlled(values) => values.pop(),
        }
    }
    fn last_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Ordinary(values) => values.last_mut(),
            Self::Controlled(values) => values.last_mut(),
        }
    }
}

enum Input<'a> {
    Values(std::slice::Iter<'a, Value>),
    Map(std::collections::btree_map::Iter<'a, String, Value>),
}

enum Sequence<'a> {
    List,
    Row(&'a RowValue),
    Record(&'a crate::RecordValue),
    Array(&'a ArrayValue, Option<LegacyVectorKind>),
}

enum Output<'source, 'control> {
    Values(ProductionVec<'control, Value>, Sequence<'source>),
    Map(BTreeMap<String, Value>),
}

struct Frame<'source, 'control> {
    input: Input<'source>,
    output: Output<'source, 'control>,
    key: Option<Produced<String>>,
    memory: Option<MemoryReservation>,
}

impl<'source, 'control> Frame<'source, 'control> {
    fn new(
        source: &'source Value,
        copier: &mut Copier<'control>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        let (input, output) = match source {
            Value::List(values) => {
                let mut output = ProductionVec::new(copier.control);
                output.reserve(values.len())?;
                (
                    Input::Values(values.iter()),
                    Output::Values(output, Sequence::List),
                )
            }
            Value::Row(row) => {
                let mut output = ProductionVec::new(copier.control);
                output.reserve(row.len())?;
                (
                    Input::Values(row.iter()),
                    Output::Values(output, Sequence::Row(row)),
                )
            }
            Value::Array(array) => {
                let mut output = ProductionVec::new(copier.control);
                output.reserve(array.elements().len())?;
                (
                    Input::Values(array.elements().iter()),
                    Output::Values(output, Sequence::Array(array, None)),
                )
            }
            Value::LegacyVector(vector) => {
                let mut output = ProductionVec::new(copier.control);
                output.reserve(vector.elements().len())?;
                (
                    Input::Values(vector.elements().iter()),
                    Output::Values(
                        output,
                        Sequence::Array(vector.as_array(), Some(vector.kind())),
                    ),
                )
            }
            Value::Record(fields) => {
                let mut output = ProductionVec::new(copier.control);
                output.reserve(fields.len())?;
                (
                    Input::Values(fields.values().iter()),
                    Output::Values(output, Sequence::Record(fields)),
                )
            }
            Value::Map(fields) => (Input::Map(fields.iter()), Output::Map(BTreeMap::new())),
            _ => return Ok(None),
        };
        Ok(Some(Self {
            input,
            output,
            key: None,
            memory: copier.control.empty_reservation(),
        }))
    }

    fn next(
        &mut self,
        copier: &mut Copier<'control>,
    ) -> Result<Option<&'source Value>, ValueRetentionError> {
        (copier.check)()?;
        let (name, value) = match &mut self.input {
            Input::Values(values) => return Ok(values.next()),
            Input::Map(fields) => fields.next().map(|(name, value)| (name.as_str(), value)),
        }
        .unzip();
        if let Some(name) = name {
            if matches!(self.output, Output::Map(_)) {
                copier.grow(&mut self.memory, size_of::<(String, Value)>())?;
            }
            self.key = Some(copier.text(name)?);
        }
        Ok(value)
    }

    fn push(
        &mut self,
        value: Produced<Value>,
        copier: &mut Copier<'control>,
    ) -> Result<(), ValueRetentionError> {
        match &mut self.output {
            Output::Values(values, _) => values.push_produced(value)?,
            Output::Map(fields) => {
                let (name, name_memory) = self.key.take().expect("copied map name").into_parts();
                let (value, memory) = value.into_parts();
                fields.insert(name, value);
                self.memory = copier.control.combine(
                    self.memory.take(),
                    copier.control.combine(name_memory, memory),
                );
            }
        }
        Ok(())
    }

    fn finish(
        mut self,
        copier: &mut Copier<'control>,
    ) -> Result<Produced<Value>, ValueRetentionError> {
        (copier.check)()?;
        let value = match self.output {
            Output::Values(values, kind) => {
                let (values, memory) = values.finish()?.into_parts();
                self.memory = copier.control.combine(self.memory.take(), memory);
                match kind {
                    Sequence::List => Value::List(values),
                    Sequence::Record(source) => {
                        let descriptor = source.descriptor().clone();
                        copier.grow(&mut self.memory, descriptor.retained_payload_bytes()?)?;
                        copier.grow(
                            &mut self.memory,
                            crate::RecordValue::retained_header_bytes(),
                        )?;
                        (copier.check)()?;
                        Value::Record(crate::RecordValue::from_validated_parts(values, descriptor))
                    }
                    Sequence::Row(source) => {
                        let descriptor = source.descriptor().cloned();
                        if let Some(descriptor) = &descriptor {
                            copier.grow(&mut self.memory, descriptor.retained_buffer_bytes()?)?;
                        }
                        copier.grow(&mut self.memory, RowValue::decoded_header_bytes())?;
                        (copier.check)()?;
                        Value::Row(RowValue::from_shared_parts(values, descriptor))
                    }
                    Sequence::Array(source, legacy_kind) => {
                        let dimensions = copier.slice(source.dimensions())?;
                        let lower_bounds = copier.slice(source.lower_bounds())?;
                        copier.grow(&mut self.memory, ArrayValue::decoded_header_bytes())?;
                        let (dimensions, dimensions_memory) = dimensions.into_parts();
                        let (lower_bounds, lower_memory) = lower_bounds.into_parts();
                        self.memory = copier.control.combine(
                            self.memory.take(),
                            copier.control.combine(dimensions_memory, lower_memory),
                        );
                        (copier.check)()?;
                        let array = ArrayValue::from_copied_parts(values, dimensions, lower_bounds)
                            .with_element_type_oid(source.element_type_oid());
                        match legacy_kind {
                            Some(kind) => Value::LegacyVector(
                                LegacyVectorValue::from_validated_array(kind, array),
                            ),
                            None => Value::Array(array),
                        }
                    }
                }
            }
            Output::Map(fields) => Value::Map(fields),
        };
        copier.control.finish(value, self.memory)
    }
}

impl Copier<'_> {
    fn grow(
        &self,
        memory: &mut Option<MemoryReservation>,
        bytes: usize,
    ) -> Result<(), ValueRetentionError> {
        let added = self.control.reserve(bytes)?;
        *memory = self.control.combine(memory.take(), added);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<Produced<String>, ValueRetentionError> {
        (self.check)()?;
        let mut copied = ProductionString::new(self.control);
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
        copied.finish()
    }

    fn slice<T: Copy>(&mut self, source: &[T]) -> Result<Produced<Vec<T>>, ValueRetentionError> {
        (self.check)()?;
        let mut copied = ProductionVec::new(self.control);
        copied.reserve(source.len())?;
        for chunk in source.chunks((4096 / size_of::<T>().max(1)).max(1)) {
            (self.check)()?;
            copied.extend_from_slice(chunk)?;
        }
        (self.check)()?;
        copied.finish()
    }

    fn scalar(&mut self, source: &Value) -> Result<Produced<Value>, ValueRetentionError> {
        (self.check)()?;
        let mut memory = self.control.empty_reservation();
        let value = match source {
            Value::Null => Value::Null,
            Value::Void => Value::Void,
            Value::Bool(value) => Value::Bool(*value),
            Value::Int(value) => Value::Int(*value),
            Value::Float(value) => Value::Float(*value),
            Value::Temporal(value) => Value::Temporal(value.clone()),
            Value::Str(text) | Value::FixedChar(text) | Value::Json(text) | Value::JsonB(text) => {
                let (text, charge) = self.text(text)?.into_parts();
                memory = self.control.combine(memory, charge);
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
                memory = self.control.combine(memory, charge);
                Value::Bytes(bytes)
            }
            Value::BitString(value) => {
                self.grow(&mut memory, crate::BitStringValue::retained_header_bytes())?;
                let (bytes, charge) = self.slice(value.as_bytes())?.into_parts();
                memory = self.control.combine(memory, charge);
                Value::BitString(
                    crate::BitStringValue::try_from_packed(bytes, value.bit_len())
                        .expect("copy preserves canonical packed bits"),
                )
            }
            Value::Decimal(value) => {
                self.grow(&mut memory, value.retained_bytes())?;
                Value::Decimal(value.clone())
            }
            Value::Datum(value) => {
                self.grow(&mut memory, value.retained_bytes())?;
                Value::Datum(value.clone())
            }
            Value::Enum(value) => {
                self.grow(&mut memory, value.retained_bytes())?;
                Value::Enum(value.clone())
            }
            Value::Array(_)
            | Value::LegacyVector(_)
            | Value::List(_)
            | Value::Row(_)
            | Value::Record(_)
            | Value::Map(_) => {
                unreachable!("container has a copy frame")
            }
        };
        let result = self.control.finish(value, memory)?;
        (self.check)()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
