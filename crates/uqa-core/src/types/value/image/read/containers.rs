//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unfinished native containers retain admitted children without recursive decoding futures.

use super::{
    Budgeted, BudgetedVec, MemoryReservation, Reader, Value, ValueImageError, ValueImageResult,
};
use crate::{
    types::OwnedValue, ArrayValue, LegacyVectorKind, LegacyVectorValue, RecordDescriptor,
    RecordFieldType, RecordValue, RowDescriptor, RowValue, ValueRetirement,
};
use std::collections::BTreeMap;

struct ArrayHeader {
    dimensions: BudgetedVec<usize>,
    bounds: BudgetedVec<i32>,
    oid: Option<u32>,
}

enum Kind {
    List,
    Array(ArrayHeader),
    Legacy(LegacyVectorKind, ArrayHeader),
    Row,
    Record {
        oid: Option<u32>,
        names: BudgetedVec<String>,
    },
    Map(BTreeMap<String, Value>),
}

pub(super) struct Container {
    kind: Kind,
    values: BudgetedVec<Value>,
    key: Option<Budgeted<String>>,
    pub(super) remaining: usize,
    memory: MemoryReservation,
}

impl Drop for Container {
    fn drop(&mut self) {
        while let Some(value) = self.values.pop() {
            drop(ValueRetirement::new(value));
        }
        if let Kind::Map(fields) = &mut self.kind {
            drop(ValueRetirement::new(Value::Map(std::mem::take(fields))));
        }
    }
}

impl ArrayHeader {
    async fn read(reader: &mut Reader<'_, '_>) -> ValueImageResult<Self> {
        let rank = reader.length().await?;
        if rank > super::super::MAX_DEPTH {
            return Err(ValueImageError::Invalid("array rank"));
        }
        let mut dimensions = BudgetedVec::new(reader.memory);
        let mut bounds = BudgetedVec::new(reader.memory);
        dimensions.reserve(rank)?;
        bounds.reserve(rank)?;
        for _ in 0..rank {
            dimensions.push(reader.length().await?)?;
            bounds.push(i32::from_le_bytes(reader.fixed().await?))?;
        }
        let oid = reader.optional_oid().await?;
        Ok(Self {
            dimensions,
            bounds,
            oid,
        })
    }

    fn finish(
        self,
        elements: Budgeted<OwnedValue>,
        reader: &Reader<'_, '_>,
    ) -> ValueImageResult<Budgeted<ArrayValue>> {
        let rank = self.dimensions.len();
        let shape = ArrayValue::decoded_shape_budgeted(
            elements.values(),
            rank,
            reader.memory,
            reader.cancellation,
        )?
        .ok_or(ValueImageError::Invalid("array shape"))?;
        if shape.as_slice() != self.dimensions.as_ref() {
            return Err(ValueImageError::Invalid("array dimensions"));
        }
        drop(shape);
        let mut memory = reader.memory.reserve(ArrayValue::decoded_header_bytes())?;
        let (elements, element_memory) = elements.into_parts();
        memory.absorb(element_memory);
        let (dimensions, dimension_memory) = self.dimensions.into_parts();
        let (bounds, bound_memory) = self.bounds.into_parts();
        memory.absorb(dimension_memory);
        memory.absorb(bound_memory);
        Ok(Budgeted::new(
            ArrayValue::from_copied_parts(elements.into_values(), dimensions, bounds)
                .with_element_type_oid(self.oid),
            memory,
        ))
    }
}

impl Container {
    pub(super) async fn open(reader: &mut Reader<'_, '_>, tag: u8) -> ValueImageResult<Self> {
        let mut kind = match tag {
            12 => Kind::Array(ArrayHeader::read(reader).await?),
            13 => {
                let kind = match reader.byte().await? {
                    0 => LegacyVectorKind::SmallInteger,
                    1 => LegacyVectorKind::Oid,
                    _ => return Err(ValueImageError::Invalid("legacy vector kind")),
                };
                Kind::Legacy(kind, ArrayHeader::read(reader).await?)
            }
            14 => Kind::List,
            15 => Kind::Row,
            16 => Kind::Record {
                oid: reader.optional_oid().await?,
                names: BudgetedVec::new(reader.memory),
            },
            17 => Kind::Map(BTreeMap::new()),
            _ => unreachable!("native container tag"),
        };
        let remaining = reader.length().await?;
        let mut values = BudgetedVec::new(reader.memory);
        let memory = if matches!(kind, Kind::Map(_)) {
            reader.entries::<(String, Value)>(remaining)?
        } else {
            values.reserve(remaining)?;
            reader.memory.empty_reservation()
        };
        if let Kind::Record { names, .. } = &mut kind {
            names.reserve(remaining)?;
        }
        Ok(Self {
            kind,
            values,
            key: None,
            remaining,
            memory,
        })
    }

    pub(super) async fn prepare(&mut self, reader: &mut Reader<'_, '_>) -> ValueImageResult<()> {
        if matches!(self.kind, Kind::Record { .. } | Kind::Map(_)) {
            let key = reader.text().await?;
            if let Kind::Map(fields) = &self.kind {
                if fields
                    .last_key_value()
                    .is_some_and(|(last, _)| last >= &*key)
                {
                    return Err(ValueImageError::Invalid("map field order"));
                }
            }
            self.key = Some(key);
        }
        Ok(())
    }

    pub(super) fn accept(&mut self, value: Budgeted<OwnedValue>) -> ValueImageResult<()> {
        let (value, memory) = value.into_parts();
        self.memory.absorb(memory);
        match &mut self.kind {
            Kind::Map(fields) => {
                let (key, memory) = self.key.take().expect("native map key").into_parts();
                self.memory.absorb(memory);
                fields.insert(key, value.into_inner());
            }
            Kind::Record { names, .. } => {
                let (key, memory) = self.key.take().expect("native record key").into_parts();
                self.memory.absorb(memory);
                names.push(key)?;
                self.values.push_owned_value(value)?;
            }
            _ => self.values.push_owned_value(value)?,
        }
        self.remaining -= 1;
        Ok(())
    }

    pub(super) async fn finish(
        mut self,
        reader: &mut Reader<'_, '_>,
    ) -> ValueImageResult<Budgeted<OwnedValue>> {
        let mut memory = std::mem::replace(&mut self.memory, reader.memory.empty_reservation());
        let (values, value_memory) =
            std::mem::replace(&mut self.values, BudgetedVec::new(reader.memory)).into_parts();
        memory.absorb(value_memory);
        let values = Budgeted::new(OwnedValue::new(Value::List(values)), memory);
        Ok(match std::mem::replace(&mut self.kind, Kind::List) {
            Kind::List => values,
            Kind::Array(header) => Reader::owned(header.finish(values, reader)?, Value::Array),
            Kind::Legacy(kind, header) => {
                let (array, memory) = header.finish(values, reader)?.into_parts();
                let control = reader.control();
                let array = crate::memory::Produced::from(Budgeted::new(array, memory));
                let vector = LegacyVectorValue::try_from_array_with_control(kind, array, &control)?
                    .ok_or(ValueImageError::Invalid("legacy vector elements"))?;
                let (vector, memory) = vector.into_parts();
                Budgeted::new(
                    OwnedValue::new(Value::LegacyVector(vector)),
                    memory.expect("admitted vector"),
                )
            }
            Kind::Row => reader.row_from(values).await?,
            Kind::Record { oid, names } => {
                let mut memory = reader
                    .memory
                    .reserve(RecordDescriptor::buffer_bytes(names.len())?)?;
                memory.grow(RecordValue::retained_header_bytes())?;
                let (values, value_memory) = values.into_parts();
                memory.absorb(value_memory);
                let (names, name_memory) = names.into_parts();
                let source_bytes = name_memory.bytes();
                memory.absorb(name_memory);
                let descriptor = RecordDescriptor::new(names, oid);
                drop(memory.split(source_bytes));
                Budgeted::new(
                    OwnedValue::new(Value::Record(RecordValue::from_validated_parts(
                        values.into_values(),
                        descriptor,
                    ))),
                    memory,
                )
            }
            Kind::Map(fields) => {
                let (_, memory) = values.into_parts();
                Budgeted::new(OwnedValue::new(Value::Map(fields)), memory)
            }
        })
    }
}

impl Reader<'_, '_> {
    async fn row_from(
        &mut self,
        values: Budgeted<OwnedValue>,
    ) -> ValueImageResult<Budgeted<OwnedValue>> {
        let field_types = if self.flag().await? {
            let mut types = BudgetedVec::new(self.memory);
            types.reserve(values.values().len())?;
            for _ in 0..values.values().len() {
                types.push(RecordFieldType {
                    oid: u32::from_le_bytes(self.fixed().await?),
                    type_modifier: i32::from_le_bytes(self.fixed().await?),
                    collation_oid: self.optional_oid().await?,
                })?;
            }
            Some(types)
        } else {
            None
        };
        let mut memory = self.memory.reserve(RowValue::decoded_header_bytes())?;
        if let Some(types) = &field_types {
            memory.grow(RowDescriptor::buffer_bytes(types.len())?)?;
        }
        let (values, value_memory) = values.into_parts();
        memory.absorb(value_memory);
        let mut type_source_bytes = 0;
        let field_types = field_types.map(|types| {
            let (types, type_memory) = types.into_parts();
            type_source_bytes = type_memory.bytes();
            memory.absorb(type_memory);
            types
        });
        let value = OwnedValue::new(Value::Row(RowValue::from_validated_parts(
            values.into_values(),
            field_types,
        )));
        drop(memory.split(type_source_bytes));
        Ok(Budgeted::new(value, memory))
    }
}
