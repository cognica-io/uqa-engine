//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field vectors preserve their original buffers and aggregate leases through SQL type retirement.

use super::{
    ownership::{RecordMemory, Storage},
    ColumnType, RecordDescriptor, RecordFieldMetadata, RecordFields, RecordMetadata,
};
use crate::{
    ir::ownership::Retirement,
    plan::retirement::{scalar_fields, Pending, Retire},
    schema::retirement::fields::Input,
};
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

pub(in crate::schema) enum Types {
    Owned {
        types: Vec<Option<ColumnType>>,
        _memory: Option<MemoryReservation>,
    },
    // A caller-supplied unsized Arc retains its public sharing contract. Its allocator-owned final release is not an admitted vector work window.
    External {
        _fields: Arc<[Option<ColumnType>]>,
    },
}
impl Retire for Types {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        match self {
            Self::Owned { types, .. } => types.retire_step(positions, scalar),
            Self::External { .. } => true,
        }
    }
}
impl RecordFields {
    fn take_retirement(&mut self) -> Option<Pending<Types>> {
        let fields = match self.0.take()? {
            Storage::Ordinary(types) => Types::External { _fields: types },
            Storage::Owned(types) => Types::Owned {
                types: Arc::into_inner(types)?,
                _memory: None,
            },
            Storage::Produced(types) => {
                let (types, memory) = Arc::into_inner(types)?.into_parts();
                Types::Owned {
                    types,
                    _memory: memory,
                }
            }
        };
        Some(Pending::new(fields))
    }
}
impl Input for RecordFields {
    type Retired = Option<Pending<Types>>;
    fn into_retirement(mut self) -> Self::Retired {
        self.take_retirement()
    }
}
impl Drop for RecordFields {
    fn drop(&mut self) {
        drop(self.take_retirement());
    }
}

impl Retire for RecordFieldMetadata {
    fn retire_step(&mut self, _: &mut [usize], _: &mut Option<Retirement>) -> bool {
        true
    }
}
pub(in crate::schema) struct Descriptor {
    types: Option<Pending<Types>>,
    names: Option<Vec<String>>,
    fields: Vec<RecordFieldMetadata>,
    _memory: RecordMemory,
}
impl Descriptor {
    fn take(value: &mut RecordDescriptor) -> Self {
        Self {
            types: value.types.take_retirement(),
            names: value.names.take().map(|names| names.into_vec()),
            fields: std::mem::take(&mut value.fields).into_vec(),
            _memory: std::mem::take(&mut value.memory),
        }
    }
}
impl Retire for Descriptor {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        scalar_fields!(positions, scalar; self.types, self.names, self.fields)
    }
}
impl Input for Arc<RecordDescriptor> {
    type Retired = Option<Pending<Descriptor>>;
    fn into_retirement(self) -> Self::Retired {
        Arc::into_inner(self).map(|mut value| Pending::new(Descriptor::take(&mut value)))
    }
}
impl Drop for RecordDescriptor {
    fn drop(&mut self) {
        drop(Pending::new(Descriptor::take(self)));
    }
}

pub(in crate::schema) struct Metadata {
    types: Option<Pending<Types>>,
    descriptor: Option<Pending<Descriptor>>,
}
impl Input for RecordMetadata {
    type Retired = Pending<Metadata>;
    fn into_retirement(self) -> Self::Retired {
        Pending::new(Metadata {
            types: self.types.and_then(Input::into_retirement),
            descriptor: self.descriptor.and_then(Input::into_retirement),
        })
    }
}
impl Retire for Metadata {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        scalar_fields!(positions, scalar; self.types, self.descriptor)
    }
}
