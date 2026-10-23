//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable anonymous-row metadata shared by its producer and values.

use std::sync::Arc;

use super::{RecordFieldType, ValueRetentionError};
use crate::memory::{MemoryError, Produced, ProductionControl};

/// Opaque field types supplied by the SQL owner. Sharing never resolves types
/// or changes their OID, modifier, collation, order or duplicate positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDescriptor(Arc<[RecordFieldType]>);

impl RowDescriptor {
    #[must_use]
    pub fn new(fields: Vec<RecordFieldType>) -> Self {
        Self(fields.into())
    }

    #[must_use]
    pub fn field_types(&self) -> &[RecordFieldType] {
        &self.0
    }

    #[must_use]
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Consuming an owned-field API creates an independent vector. Producers
    /// that retain metadata should pass the descriptor itself instead.
    #[must_use]
    pub fn into_field_types(self) -> Vec<RecordFieldType> {
        self.0.to_vec()
    }

    /// Logical retention charges include this metadata for each admitted row,
    /// even when its physical allocation is shared. Reference-count allocator
    /// bookkeeping follows the existing retained-value accounting boundary.
    pub fn retained_buffer_bytes(&self) -> Result<usize, MemoryError> {
        Self::buffer_bytes(self.0.len())
    }

    pub(in crate::types) fn buffer_bytes(width: usize) -> Result<usize, MemoryError> {
        width
            .checked_mul(size_of::<RecordFieldType>())
            .ok_or(MemoryError::SizeOverflow)
    }

    /// Compact admitted field types into shared immutable storage. Reserve its
    /// destination before allocation and release the temporary vector lease.
    pub fn with_control(
        fields: Produced<Vec<RecordFieldType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let retained = control.reserve(Self::buffer_bytes(fields.len())?)?;
        let (fields, scratch) = fields.into_parts();
        let descriptor = Self::new(fields);
        drop(scratch);
        control.finish(descriptor, retained)
    }
}

#[cfg(test)]
mod tests;
