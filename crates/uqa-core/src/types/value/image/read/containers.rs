//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::{Budgeted, BudgetedVec, Reader, Value, ValueImageError, ValueImageResult};
use crate::{
    ArrayValue, LegacyVectorKind, LegacyVectorValue, RecordDescriptor, RecordFieldType,
    RecordValue, RowDescriptor, RowValue,
};

impl Reader<'_> {
    pub(super) fn array(&mut self, depth: usize) -> ValueImageResult<Budgeted<ArrayValue>> {
        let rank = self.length()?;
        if rank > super::super::MAX_DEPTH {
            return Err(ValueImageError::Invalid("array rank"));
        }
        let mut dimensions = BudgetedVec::new(self.memory);
        let mut bounds = BudgetedVec::new(self.memory);
        dimensions.reserve(rank)?;
        bounds.reserve(rank)?;
        for _ in 0..rank {
            dimensions.push(self.length()?)?;
            bounds.push(i32::from_le_bytes(self.fixed()?))?;
        }
        let oid = self.optional_oid()?;
        let elements = self.values(depth)?;
        let shape =
            ArrayValue::decoded_shape_budgeted(&elements, rank, self.memory, self.cancellation)?
                .ok_or(ValueImageError::Invalid("array shape"))?;
        if shape.as_slice() != dimensions.as_ref() {
            return Err(ValueImageError::Invalid("array dimensions"));
        }
        drop(shape);
        let (elements, mut memory) = elements.into_parts();
        let (dimensions, dimension_memory) = dimensions.into_parts();
        let (bounds, bound_memory) = bounds.into_parts();
        memory.absorb(dimension_memory);
        memory.absorb(bound_memory);
        memory.grow(ArrayValue::decoded_header_bytes())?;
        Ok(Budgeted::new(
            ArrayValue::from_copied_parts(elements, dimensions, bounds).with_element_type_oid(oid),
            memory,
        ))
    }

    pub(super) fn legacy_vector(&mut self, depth: usize) -> ValueImageResult<Budgeted<Value>> {
        let kind = match self.byte()? {
            0 => LegacyVectorKind::SmallInteger,
            1 => LegacyVectorKind::Oid,
            _ => return Err(ValueImageError::Invalid("legacy vector kind")),
        };
        let (array, memory) = self.array(depth)?.into_parts();
        let control = self.control();
        let array = control.finish(array, Some(memory))?;
        let vector = LegacyVectorValue::try_from_array_with_control(kind, array, &control)?
            .ok_or(ValueImageError::Invalid("legacy vector elements"))?;
        let (vector, memory) = vector.into_parts();
        Ok(Budgeted::new(
            Value::LegacyVector(vector),
            memory.expect("admitted vector"),
        ))
    }

    pub(super) fn row(&mut self, depth: usize) -> ValueImageResult<Budgeted<Value>> {
        let (values, mut memory) = self.values(depth)?.into_parts();
        let mut type_source_bytes = 0;
        let field_types = if self.flag()? {
            let mut types = BudgetedVec::new(self.memory);
            types.reserve(values.len())?;
            for _ in 0..values.len() {
                types.push(RecordFieldType {
                    oid: u32::from_le_bytes(self.fixed()?),
                    type_modifier: i32::from_le_bytes(self.fixed()?),
                    collation_oid: self.optional_oid()?,
                })?;
            }
            memory.grow(RowDescriptor::buffer_bytes(types.len())?)?;
            let (types, type_memory) = types.into_parts();
            type_source_bytes = type_memory.bytes();
            memory.absorb(type_memory);
            Some(types)
        } else {
            None
        };
        memory.grow(RowValue::decoded_header_bytes())?;
        let value = Value::Row(RowValue::from_validated_parts(values, field_types));
        drop(memory.split(type_source_bytes));
        Ok(Budgeted::new(value, memory))
    }

    pub(super) fn record(&mut self, depth: usize) -> ValueImageResult<Budgeted<Value>> {
        let oid = self.optional_oid()?;
        let count = self.length()?;
        let mut memory = self.memory.empty_reservation();
        let mut names = BudgetedVec::new(self.memory);
        let mut values = BudgetedVec::new(self.memory);
        names.reserve(count)?;
        values.reserve(count)?;
        for _ in 0..count {
            let (name, name_memory) = self.text()?.into_parts();
            names.push(name)?;
            memory.absorb(name_memory);
            let (value, value_memory) = self.value(depth + 1)?.into_parts();
            values.push(value)?;
            memory.absorb(value_memory);
        }
        memory.grow(RecordDescriptor::buffer_bytes(count)?)?;
        memory.grow(RecordValue::retained_header_bytes())?;
        let (names, name_memory) = names.into_parts();
        let name_source_bytes = name_memory.bytes();
        let (values, value_memory) = values.into_parts();
        memory.absorb(name_memory);
        memory.absorb(value_memory);
        let descriptor = RecordDescriptor::new(names, oid);
        drop(memory.split(name_source_bytes));
        Ok(Budgeted::new(
            Value::Record(RecordValue::from_validated_parts(values, descriptor)),
            memory,
        ))
    }

    pub(super) fn map(&mut self, depth: usize) -> ValueImageResult<Budgeted<Value>> {
        let count = self.length()?;
        let mut memory = self.entries::<(String, Value)>(count)?;
        let mut fields = BTreeMap::new();
        for _ in 0..count {
            let (name, name_memory) = self.text()?.into_parts();
            if fields
                .last_key_value()
                .is_some_and(|(last, _)| last >= &name)
            {
                return Err(ValueImageError::Invalid("map field order"));
            }
            let (value, value_memory) = self.value(depth + 1)?.into_parts();
            fields.insert(name, value);
            memory.absorb(name_memory);
            memory.absorb(value_memory);
        }
        Ok(Budgeted::new(Value::Map(fields), memory))
    }
}
