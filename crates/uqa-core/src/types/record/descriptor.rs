//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable named-record metadata. SQL owns the meaning of its opaque OID.

use crate::memory::{MemoryError, Produced, ProductionControl};
use crate::ValueRetentionError;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordDescriptor {
    names: Arc<FieldNames>,
    type_oid: Option<u32>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct FieldNames {
    names: Box<[String]>,
}

impl RecordDescriptor {
    #[must_use]
    pub fn new(names: Vec<String>, type_oid: Option<u32>) -> Self {
        Self {
            names: Arc::new(FieldNames {
                names: names.into_boxed_slice(),
            }),
            type_oid,
        }
    }

    #[must_use]
    pub fn field_names(&self) -> &[String] {
        &self.names.names
    }

    #[must_use]
    pub fn type_oid(&self) -> Option<u32> {
        self.type_oid
    }

    #[must_use]
    pub fn with_type_oid(mut self, type_oid: Option<u32>) -> Self {
        self.type_oid = type_oid;
        self
    }

    #[must_use]
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.names, &other.names)
    }

    /// Explicitly obtain independently mutable names at an owned API boundary.
    #[must_use]
    pub fn into_field_names(self) -> Vec<String> {
        match Arc::try_unwrap(self.names) {
            Ok(names) => names.names.into_vec(),
            Err(shared) => shared.names.to_vec(),
        }
    }

    pub(in crate::types) fn buffer_bytes(width: usize) -> Result<usize, MemoryError> {
        Self::field_bytes(width)?
            .checked_add(size_of::<FieldNames>())
            .ok_or(MemoryError::SizeOverflow)
    }

    pub(in crate::types) fn field_bytes(width: usize) -> Result<usize, MemoryError> {
        width
            .checked_mul(size_of::<String>())
            .ok_or(MemoryError::SizeOverflow)
    }

    pub fn retained_buffer_bytes(&self) -> Result<usize, MemoryError> {
        Self::buffer_bytes(self.names.names.len())
    }

    /// Logical admission includes all names even when their allocation is shared.
    pub fn retained_payload_bytes(&self) -> Result<usize, MemoryError> {
        self.names
            .names
            .iter()
            .try_fold(self.retained_buffer_bytes()?, |bytes, name| {
                bytes
                    .checked_add(name.capacity())
                    .ok_or(MemoryError::SizeOverflow)
            })
    }

    /// Compact admitted names without copying string payloads. Both source and
    /// destination headers are admitted while the conversion is in flight.
    pub fn with_control(
        names: Produced<Vec<String>>,
        type_oid: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let retained = control.reserve(Self::buffer_bytes(names.len())?)?;
        let source_bytes = Self::field_bytes(names.capacity())?;
        let (names, mut memory) = names.into_parts();
        let descriptor = Self::new(names, type_oid);
        if let Some(memory) = &mut memory {
            drop(memory.split(source_bytes));
        }
        control.finish(descriptor, control.combine(retained, memory))
    }
}

impl crate::memory::ObserveAllocations for RecordDescriptor {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut crate::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        if observer.shared_owner(self.names.as_ref())? {
            observer.allocation(size_of::<FieldNames>(), size_of::<FieldNames>())?;
            observer.buffer::<String>(self.names.names.len(), self.names.names.len())?;
            for name in &self.names.names {
                observer.child(name)?;
            }
        }
        Ok(())
    }
}
