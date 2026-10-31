//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field arrays and selectable record descriptors retain their own leases independently of every schema that references them.

use super::{ColumnType, RecordDescriptor, RecordFieldMetadata};
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl, ProductionVec};

#[derive(Clone)]
pub struct RecordFields(Storage);
#[derive(Clone)]
enum Storage {
    Ordinary(Arc<[Option<ColumnType>]>),
    Produced(Arc<Produced<Vec<Option<ColumnType>>>>),
}
impl RecordFields {
    pub fn from_produced(
        types: Produced<Vec<Option<ColumnType>>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(Storage::Produced(types.into_shared(control)?)))
    }
    pub fn from_types_with_control<'a>(
        types: impl IntoIterator<Item = Option<&'a ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut output = ProductionVec::new(*control);
        for ty in types {
            let ty = if let Some(ty) = ty {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                control.finish(Some(ty), memory)?
            } else {
                control.finish(None, control.empty_reservation())?
            };
            output.push_produced(ty)?;
        }
        Self::from_produced(output.finish()?, control)
    }
    pub fn shares_storage(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Storage::Ordinary(left), Storage::Ordinary(right)) => Arc::ptr_eq(left, right),
            (Storage::Produced(left), Storage::Produced(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}
impl std::ops::Deref for RecordFields {
    type Target = [Option<ColumnType>];
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Storage::Ordinary(types) => types,
            Storage::Produced(types) => types,
        }
    }
}
impl AsRef<[Option<ColumnType>]> for RecordFields {
    fn as_ref(&self) -> &[Option<ColumnType>] {
        self
    }
}
impl std::fmt::Debug for RecordFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_ref().fmt(f)
    }
}
impl PartialEq for RecordFields {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl Eq for RecordFields {}
impl From<Vec<Option<ColumnType>>> for RecordFields {
    fn from(types: Vec<Option<ColumnType>>) -> Self {
        Self(Storage::Ordinary(types.into()))
    }
}
impl From<Arc<[Option<ColumnType>]>> for RecordFields {
    fn from(types: Arc<[Option<ColumnType>]>) -> Self {
        Self(Storage::Ordinary(types))
    }
}
impl FromIterator<Option<ColumnType>> for RecordFields {
    fn from_iter<I: IntoIterator<Item = Option<ColumnType>>>(types: I) -> Self {
        types.into_iter().collect::<Vec<_>>().into()
    }
}

#[derive(Debug, Default)]
pub(crate) struct RecordMemory {
    _memory: Option<MemoryReservation>,
}
impl PartialEq for RecordMemory {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for RecordMemory {}

impl RecordDescriptor {
    pub(crate) fn from_parts_with_control(
        types: RecordFields,
        names: Option<Produced<Vec<String>>>,
        fields: Produced<Vec<RecordFieldMetadata>>,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Self>, SQLError> {
        control.check()?;
        let names = names
            .map(|names| crate::schema::production::lease::boxed_slice(names, control))
            .transpose()?;
        let fields = crate::schema::production::lease::boxed_slice(fields, control)?;
        let header = control.reserve(size_of::<Self>())?;
        let (names, names_memory) = match names {
            Some(names) => {
                let (names, memory) = names.into_parts();
                (Some(names), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let (fields, fields_memory) = fields.into_parts();
        Ok(Arc::new(Self {
            types,
            names,
            fields,
            memory: RecordMemory {
                _memory: control.combine(header, control.combine(names_memory, fields_memory)),
            },
        }))
    }
}
